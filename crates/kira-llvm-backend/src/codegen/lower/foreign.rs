//! Foreign call lowering: marshalling a Kira call site to libffi.
//!
//! Arguments are copied into exact C storage, a shared recursive descriptor
//! selects libffi's CIF, and the checked result is read back as a Kira value.

use kira_ir::IrExprId;
use kira_runtime_abi::{ForeignAdapterStatus, ForeignPointerWidth, ForeignType, ForeignTypeSpec};
use kira_semantics_model::Type;
use llvm_sys::LLVMIntPredicate;
use llvm_sys::core::*;
use llvm_sys::prelude::*;

use super::FunctionLowering;
use crate::LlvmError;

impl FunctionLowering<'_, '_> {
    /// Lowers a call to foreign import `index` through the bundled libffi path.
    pub(super) fn lower_foreign_call(
        &mut self,
        index: u32,
        args: &[IrExprId],
        result_ty: Type,
    ) -> Result<LLVMValueRef, LlvmError> {
        let idx = index as usize;
        let import = self.codegen.program.foreign_imports[idx].import.clone();
        // A system call is reached by an instruction rather than by a symbol, so
        // none of the marshalling below applies to it: nothing is bound, nothing
        // is looked up, and its arguments are register words rather than C
        // storage libffi is handed the address of.
        if !import.abi().binds_a_library_symbol() {
            return self.lower_syscall_call(index, args, result_ty);
        }
        // A data symbol is bound and then nothing is invoked: the answer is
        // where the object is, which is a link-time constant. Everything below
        // marshals arguments into C storage and builds a CIF, and doing any of
        // it here would call the object's first bytes.
        if import.abi().answers_an_address() {
            let symbol = self.codegen.declare_foreign_data(import.symbol());
            // A Kira `RawPtr` is an integer at this layer, so the symbol's
            // address crosses as one. Handing back the global itself types the
            // call site as `ptr` and the module is rejected.
            // SAFETY: `symbol` is a global of this live module and the builder
            // is positioned in the current block.
            return Ok(unsafe {
                LLVMBuildPtrToInt(
                    self.codegen.builder,
                    symbol,
                    self.codegen.types.i64,
                    c"foreign.address".as_ptr(),
                )
            });
        }
        let signature = import.signature();
        let params = signature.parameters().to_vec();
        let retained: Vec<bool> = (0..params.len())
            .map(|position| signature.is_retained(position))
            .collect();
        let result_spec = signature.result();

        if self.codegen.unavailable.contains(&idx) {
            // The trap helper reads these as C strings, so they are terminated
            // here. A length-carrying Kira constant has no terminator, and the
            // message then printed the library glued to whatever constant the
            // module laid down after it.
            let library = self.codegen.c_string_constant(import.library());
            let symbol = self.codegen.c_string_constant(import.symbol());
            self.call(
                self.codegen.runtime.trap_foreign_unavailable,
                &mut [library, symbol],
                c"",
            );
            // What follows this call site still lowers somewhere, and a
            // terminated block takes no more instructions.
            let function = self.current_function();
            let unreached = self.append_block(function, c"foreign.unavailable");
            // SAFETY: the trap never returns; `unreached` belongs to this
            // function and no value out of it is observed.
            let undef = unsafe {
                LLVMBuildUnreachable(self.codegen.builder);
                LLVMGetUndef(self.codegen.llvm_type(result_ty)?)
            };
            self.position_at(unreached);
            return Ok(undef);
        }

        // Arguments evaluate left to right, as the VM pushes them.
        let mut values = Vec::with_capacity(args.len());
        for &argument in args {
            let ty = self.type_of(argument);
            values.push((self.lower_expr(argument)?, ty));
        }

        let types = self.codegen.types;
        let builder = self.codegen.builder;
        let saved = self.call(self.codegen.runtime.stack_save, &mut [], c"stack.save");
        let mut argument_pointers = Vec::with_capacity(values.len());
        let mut cstring_pointers = Vec::new();

        for ((value, ty), spec) in values.iter().copied().zip(params.iter().copied()) {
            let pointer = match spec {
                ForeignTypeSpec::Aggregate(id) => self.write_aggregate_buffer(id, value, ty)?,
                ForeignTypeSpec::Scalar(ForeignType::Void) => {
                    return Err(LlvmError::internal(
                        "a void parameter reached the native libffi path",
                    ));
                }
                ForeignTypeSpec::Scalar(ForeignType::CString) => {
                    let c_string = if ty == Type::CBlock {
                        let word = self.call(
                            self.codegen.runtime.cblock_word,
                            &mut [value],
                            c"ffi.cblock.word",
                        );
                        // SAFETY: a C-block word is the target pointer encoded
                        // in Kira's i64 seam representation.
                        unsafe {
                            LLVMBuildIntToPtr(builder, word, types.ptr, c"ffi.cblock.ptr".as_ptr())
                        }
                    } else {
                        let c_string = self.call(
                            self.codegen.runtime.cstring_new,
                            &mut [value],
                            c"ffi.cstring.new",
                        );
                        cstring_pointers.push(c_string);
                        c_string
                    };
                    // SAFETY: the builder is positioned in this function and
                    // both operands are values of this module's pointer type.
                    let is_null = unsafe {
                        LLVMBuildICmp(
                            builder,
                            LLVMIntPredicate::LLVMIntEQ,
                            c_string,
                            LLVMConstPointerNull(types.ptr),
                            c"ffi.cstring.null".as_ptr(),
                        )
                    };
                    let function = self.current_function();
                    let bad = self.append_block(function, c"ffi.cstring.bad");
                    let good = self.append_block(function, c"ffi.cstring.good");
                    // SAFETY: both blocks belong to the active function and the
                    // current block is unterminated.
                    unsafe { LLVMBuildCondBr(builder, is_null, bad, good) };
                    self.position_at(bad);
                    // SAFETY: the integer type belongs to this live module.
                    let status = unsafe {
                        LLVMConstInt(
                            types.i32,
                            u64::from(ForeignAdapterStatus::INTERIOR_NUL.0),
                            0,
                        )
                    };
                    self.call(self.codegen.runtime.trap_foreign, &mut [status], c"");
                    // SAFETY: the foreign trap never returns.
                    unsafe { LLVMBuildUnreachable(builder) };
                    self.position_at(good);
                    // SAFETY: the builder is positioned in this function and the
                    // pointer type belongs to this live module.
                    let storage =
                        unsafe { LLVMBuildAlloca(builder, types.ptr, c"ffi.cstring.arg".as_ptr()) };
                    let layout =
                        kira_runtime_abi::scalar_layout(ForeignType::CString, self.pointer_width());
                    // Libffi receives an array of addresses to argument
                    // values, not the argument values themselves. Keep the
                    // transient C pointer in pointer-sized storage so the
                    // foreign call reads the pointer rather than the first
                    // bytes of the pointed-to string.
                    // SAFETY: `storage` is the alloca above, sized and aligned
                    // for one C pointer, which is what this stores into it.
                    unsafe {
                        LLVMSetAlignment(storage, layout.align);
                        let store = LLVMBuildStore(builder, c_string, storage);
                        LLVMSetAlignment(store, layout.align);
                    }
                    storage
                }
                ForeignTypeSpec::Scalar(ft) => {
                    let layout = kira_runtime_abi::scalar_layout(ft, self.pointer_width());
                    // SAFETY: the builder is positioned in this function and the
                    // C type belongs to this live module.
                    let storage = unsafe {
                        LLVMBuildAlloca(
                            builder,
                            self.codegen.foreign_c_type(ft),
                            c"ffi.arg".as_ptr(),
                        )
                    };
                    // SAFETY: the alloca and store use the scalar's shared C
                    // alignment, so libffi sees correctly aligned storage.
                    unsafe { LLVMSetAlignment(storage, layout.align) };
                    let value = if ty == Type::CBlock {
                        self.call(
                            self.codegen.runtime.cblock_word,
                            &mut [value],
                            c"ffi.cblock.word",
                        )
                    } else {
                        value
                    };
                    let converted = self.codegen.kira_value_to_c(value, ft)?;
                    // SAFETY: `storage` is the alloca above, whose type is this
                    // scalar's own C type.
                    let store = unsafe { LLVMBuildStore(builder, converted, storage) };
                    // SAFETY: `store` is that instruction, aligned as its type.
                    unsafe { LLVMSetAlignment(store, layout.align) };
                    storage
                }
            };
            argument_pointers.push(pointer);
        }

        let argument_array = if argument_pointers.is_empty() {
            // SAFETY: the pointer type belongs to this live module context.
            unsafe { LLVMConstPointerNull(types.ptr) }
        } else {
            // SAFETY: the builder is positioned in this function and both the
            // pointer and integer types belong to this live module.
            let array = unsafe {
                LLVMBuildArrayAlloca(
                    builder,
                    types.ptr,
                    LLVMConstInt(types.i64, argument_pointers.len() as u64, 0),
                    c"ffi.args".as_ptr(),
                )
            };
            for (index, pointer) in argument_pointers.iter().copied().enumerate() {
                // SAFETY: `array` is the alloca above and the index is within
                // the length it was allocated with.
                let element = unsafe {
                    let mut offset = [LLVMConstInt(types.i64, index as u64, 0)];
                    LLVMBuildInBoundsGEP2(
                        builder,
                        types.ptr,
                        array,
                        offset.as_mut_ptr(),
                        1,
                        c"ffi.arg.ptr".as_ptr(),
                    )
                };
                // SAFETY: `element` is one slot in the argument pointer array.
                unsafe { LLVMBuildStore(builder, pointer, element) };
            }
            array
        };

        let result_storage = match result_spec {
            ForeignTypeSpec::Scalar(ForeignType::Void) => {
                // SAFETY: the pointer type belongs to this live module context.
                unsafe { LLVMConstPointerNull(types.ptr) }
            }
            ForeignTypeSpec::Aggregate(id) => self.aggregate_alloca(id)?,
            ForeignTypeSpec::Scalar(ft) => {
                let layout = kira_runtime_abi::scalar_layout(ft, self.pointer_width());
                // SAFETY: the builder is positioned in this function and the C
                // type belongs to this live module.
                let storage = unsafe {
                    LLVMBuildAlloca(
                        builder,
                        self.codegen.foreign_c_type(ft),
                        c"ffi.result".as_ptr(),
                    )
                };
                // SAFETY: this alloca is the result storage for the same scalar
                // layout the descriptor hands to libffi.
                unsafe { LLVMSetAlignment(storage, layout.align) };
                storage
            }
        };

        if self.codegen.calls_foreign_directly() {
            self.emit_direct_foreign_call(
                import.symbol(),
                &params,
                result_spec,
                &argument_pointers,
                result_storage,
            )?;
            // The call owns the values its argument expressions produced.
            for (position, (value, ty)) in values.into_iter().enumerate() {
                self.release_foreign_argument(
                    value,
                    ty,
                    retained.get(position).copied().unwrap_or(false),
                )?;
            }
        } else {
            let function = self.codegen.declare_foreign_address(import.symbol());
            let descriptor = self.codegen.foreign_ffi_descriptor(idx)?;
            let mut call_args = [function, descriptor, argument_array, result_storage];
            let status = self.call(
                self.codegen.runtime.ffi_call,
                &mut call_args,
                c"foreign.status",
            );

            let function = self.current_function();
            let ok = self.append_block(function, c"foreign.ok");
            let fail = self.append_block(function, c"foreign.fail");
            // SAFETY: the builder is on the call's block and `status` is the
            // helper's i32 return value.
            unsafe {
                let failed = LLVMBuildICmp(
                    builder,
                    LLVMIntPredicate::LLVMIntNE,
                    status,
                    LLVMConstInt(types.i32, 0, 0),
                    c"foreign.failed".as_ptr(),
                );
                LLVMBuildCondBr(builder, failed, fail, ok);
            }
            self.position_at(fail);
            // A refused adapter call showed C no retained pointer. Release all
            // evaluated arguments before the trap rather than transferring any.
            for (value, ty) in values.iter().copied() {
                self.release_foreign_argument(value, ty, false)?;
            }
            self.call(self.codegen.runtime.trap_foreign, &mut [status], c"");
            // SAFETY: `kira_rt_trap_foreign` never returns.
            unsafe { LLVMBuildUnreachable(builder) };
            self.position_at(ok);
            for (position, (value, ty)) in values.into_iter().enumerate() {
                self.release_foreign_argument(
                    value,
                    ty,
                    retained.get(position).copied().unwrap_or(false),
                )?;
            }
        }

        // Lift before freeing transient C strings: a C function may return a
        // pointer into one of its input strings.
        let value = match result_spec {
            ForeignTypeSpec::Aggregate(id) => {
                self.read_aggregate_buffer(id, result_storage, result_ty)?
            }
            ForeignTypeSpec::Scalar(ft) => {
                self.codegen.read_raw_foreign_result(result_storage, ft)?
            }
        };
        for pointer in cstring_pointers {
            self.call(
                self.codegen.runtime.cstring_free,
                &mut [pointer],
                c"ffi.cstring.free",
            );
        }
        self.call(self.codegen.runtime.stack_restore, &mut [saved], c"");
        Ok(value)
    }

    /// Releases one evaluated foreign argument or transfers its C-block tree.
    fn release_foreign_argument(
        &mut self,
        value: LLVMValueRef,
        ty: Type,
        retained: bool,
    ) -> Result<(), LlvmError> {
        if retained && self.codegen.contains_c_storage(ty) {
            return self.keep_c_storage(value, ty);
        }
        self.drop_value(value, ty)
    }

    /// Transfers every C block reachable from `value` to the native registry.
    fn keep_c_storage(&mut self, value: LLVMValueRef, ty: Type) -> Result<(), LlvmError> {
        if ty == Type::CBlock {
            self.call(self.codegen.runtime.cblock_keep, &mut [value], c"");
            return Ok(());
        }
        let llvm_type = self.codegen.llvm_type(ty)?;
        let (source, saved) = self.codegen.dynamic_alloca(llvm_type, c"retained.source");
        // SAFETY: `source` is one writable slot of `llvm_type`.
        unsafe { LLVMBuildStore(self.codegen.builder, value, source) };
        self.keep_c_storage_at(source, ty)?;
        self.codegen.release_at(source, ty)?;
        self.codegen.release_dynamic_alloca(source, saved);
        Ok(())
    }

    /// Moves reachable C-block handles out of `at` into the registry.
    fn keep_c_storage_at(&mut self, at: LLVMValueRef, ty: Type) -> Result<(), LlvmError> {
        match ty {
            Type::CBlock => {
                // SAFETY: `at` addresses one C-block handle.
                let handle = unsafe {
                    LLVMBuildLoad2(
                        self.codegen.builder,
                        self.codegen.types.i64,
                        at,
                        c"keep".as_ptr(),
                    )
                };
                self.call(self.codegen.runtime.cblock_keep, &mut [handle], c"");
                // SAFETY: ownership moved to the registry; null keeps the later
                // release walk from freeing it a second time.
                unsafe { LLVMBuildStore(self.codegen.builder, self.codegen.const_int(0), at) };
                Ok(())
            }
            Type::Struct(id) => {
                let def = self
                    .codegen
                    .program
                    .types
                    .structs()
                    .get(id)
                    .ok_or(LlvmError::internal("a retained struct not in the table"))?
                    .clone();
                let struct_type = self.codegen.llvm_type(ty)?;
                for (index, field_ty) in def.fields.iter().map(|field| field.ty).enumerate() {
                    let field = self.codegen.field_pointer(struct_type, at, index as u32);
                    if def.owns_c_storage_at(index as u32) {
                        self.keep_c_storage_at(field, Type::CBlock)?;
                    } else if self.codegen.contains_c_storage(field_ty) {
                        self.keep_c_storage_at(field, field_ty)?;
                    }
                }
                Ok(())
            }
            Type::Array(_) => {
                let element = self.codegen.element_of(ty)?;
                if !self.codegen.contains_c_storage(element) {
                    return Ok(());
                }
                // SAFETY: `at` addresses one array handle.
                let array = unsafe {
                    LLVMBuildLoad2(
                        self.codegen.builder,
                        self.codegen.types.ptr,
                        at,
                        c"keep.array".as_ptr(),
                    )
                };
                let len = self.call(self.codegen.runtime.array_len, &mut [array], c"keep.len");
                let esize = self.codegen.abi_size(element)?;
                let clone = self.codegen.element_clone(element)?;
                self.emit_index_loop(len, |lowering, index| {
                    let slot = lowering.call(
                        lowering.codegen.runtime.array_slot_mut,
                        &mut [at, index, esize, clone],
                        c"keep.slot",
                    );
                    lowering.keep_c_storage_at(slot, element)
                })
            }
            _ => Ok(()),
        }
    }

    /// Calls the declared symbol itself, for a target with no run-time loader.
    ///
    /// The C is linked into the module, so the address is a real function and
    /// its ABI is the one the target's own compiler applied to the archive. The
    /// wasm rules are the whole of it: a struct crosses behind a pointer, and a
    /// struct with one scalar member crosses as that member.
    fn emit_direct_foreign_call(
        &mut self,
        symbol: &str,
        params: &[ForeignTypeSpec],
        result_spec: ForeignTypeSpec,
        argument_pointers: &[LLVMValueRef],
        result_storage: LLVMValueRef,
    ) -> Result<(), LlvmError> {
        let builder = self.codegen.builder;
        let mut call_types = Vec::with_capacity(params.len() + 1);
        let mut call_args = Vec::with_capacity(params.len() + 1);
        // The C ABI extension each position carries, by LLVM attribute index.
        // Collected while the argument list is built and applied to the call
        // once it exists, because an attribute belongs to the call instruction.
        let mut extensions: Vec<(u32, ForeignType)> = Vec::new();
        let indirect_result = match result_spec {
            ForeignTypeSpec::Aggregate(id) => self.codegen.single_scalar_member(id)?.is_none(),
            ForeignTypeSpec::Scalar(_) => false,
        };
        if indirect_result {
            call_types.push(self.codegen.types.ptr);
            call_args.push(result_storage);
        }
        for (pointer, spec) in argument_pointers
            .iter()
            .copied()
            .zip(params.iter().copied())
        {
            let crossing = match spec {
                ForeignTypeSpec::Aggregate(id) => self.codegen.single_scalar_member(id)?,
                ForeignTypeSpec::Scalar(ty) => Some(ty),
            };
            match crossing {
                None => {
                    call_types.push(self.codegen.types.ptr);
                    call_args.push(pointer);
                }
                Some(ty) => {
                    let c_type = self.codegen.foreign_c_type(ty);
                    // SAFETY: the storage was written with this argument's own
                    // C layout, which is what is loaded back out of it.
                    let value =
                        unsafe { LLVMBuildLoad2(builder, c_type, pointer, c"ffi.direct".as_ptr()) };
                    // A `_Bool` object is a byte and a `_Bool` parameter is an
                    // `i1`; every other scalar's prototype type is its storage
                    // type, so only this one narrows.
                    let value = self.codegen.c_storage_to_prototype(value, ty);
                    extensions.push((call_types.len() as u32 + 1, ty));
                    call_types.push(self.codegen.foreign_c_prototype_type(ty));
                    call_args.push(value);
                }
            }
        }
        let returned = match (indirect_result, result_spec) {
            (true, _) | (_, ForeignTypeSpec::Scalar(ForeignType::Void)) => None,
            (false, ForeignTypeSpec::Aggregate(id)) => Some(
                self.codegen
                    .single_scalar_member(id)?
                    .ok_or(LlvmError::internal("a direct aggregate with no scalar"))?,
            ),
            (false, ForeignTypeSpec::Scalar(ty)) => Some(ty),
        };
        let return_type = match returned {
            None => self.codegen.types.void,
            Some(ty) => self.codegen.foreign_c_prototype_type(ty),
        };
        // SAFETY: every type belongs to this module's context and the argument
        // arrays outlive the calls below.
        let produced = unsafe {
            let function_type = LLVMFunctionType(
                return_type,
                call_types.as_mut_ptr(),
                call_types.len() as u32,
                0,
            );
            let name = crate::codegen::ffi::c_string(symbol);
            let existing = LLVMGetNamedFunction(self.codegen.module, name.as_ptr());
            let callee = if existing.is_null() {
                LLVMAddFunction(self.codegen.module, name.as_ptr(), function_type)
            } else {
                // Two same-named imports at different signatures — or an
                // import colliding with a maths declaration — would build a
                // call whose type disagrees with the definition. Name the
                // collision rather than fail verification far from the cause.
                let found = LLVMGlobalGetValueType(existing);
                if found != function_type {
                    return Err(LlvmError::SymbolCollision {
                        symbol: symbol.to_owned(),
                    });
                }
                existing
            };
            LLVMBuildCall2(
                builder,
                function_type,
                callee,
                call_args.as_mut_ptr(),
                call_args.len() as u32,
                if return_type == self.codegen.types.void {
                    c"".as_ptr()
                } else {
                    c"ffi.direct.result".as_ptr()
                },
            )
        };
        for (index, ty) in extensions {
            self.codegen.add_c_extension(produced, index, ty);
        }
        if let Some(ty) = returned {
            self.codegen.add_c_extension(produced, 0, ty);
            // The result comes back in its prototype type and is written into
            // storage sized for the C object, so a `_Bool` widens from the `i1`
            // C answered with to the canonical byte the read expects.
            let produced = self.codegen.c_prototype_to_storage(produced, ty);
            // SAFETY: `result_storage` is the C-layout storage this result is
            // read back out of, and `produced` has the result's own C type.
            unsafe { LLVMBuildStore(builder, produced, result_storage) };
        }
        Ok(())
    }

    /// The layout width the host this build targets uses.
    pub(super) fn pointer_width(&self) -> ForeignPointerWidth {
        self.codegen.pointer_width
    }

    /// An alloca sized and aligned for one aggregate's C layout.
    pub(super) fn aggregate_alloca(
        &mut self,
        id: kira_runtime_abi::ForeignAggregateId,
    ) -> Result<LLVMValueRef, LlvmError> {
        let layout = self
            .codegen
            .program
            .foreign_aggregates
            .layout_of(id, self.pointer_width())
            .map_err(|_| LlvmError::internal("an aggregate with no computable C layout"))?;
        let types = self.codegen.types;
        let builder = self.codegen.builder;
        // SAFETY: the builder is on a live block and `types.i8` belongs to this
        // module's context; the array length is the aggregate's own `sizeof`.
        Ok(unsafe {
            let count = LLVMConstInt(types.i64, u64::from(layout.size), 0);
            let buffer = LLVMBuildArrayAlloca(builder, types.i8, count, c"foreign.agg".as_ptr());
            LLVMSetAlignment(buffer, layout.align);
            buffer
        })
    }
}
