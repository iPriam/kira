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
        let signature = import.signature();
        let params = signature.parameters().to_vec();
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
                    let c_string = self.call(
                        self.codegen.runtime.cstring_new,
                        &mut [value],
                        c"ffi.cstring.new",
                    );
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
                    cstring_pointers.push(c_string);
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
            for (value, ty) in values {
                self.drop_value(value, ty)?;
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

            // The call owns the values its argument expressions produced. The C
            // storage is separate, so Kira values can be released before lifting.
            for (value, ty) in values {
                self.drop_value(value, ty)?;
            }

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
            self.call(self.codegen.runtime.trap_foreign, &mut [status], c"");
            // SAFETY: `kira_rt_trap_foreign` never returns.
            unsafe { LLVMBuildUnreachable(builder) };
            self.position_at(ok);
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
                    call_types.push(c_type);
                    call_args.push(value);
                }
            }
        }
        let return_type = match (indirect_result, result_spec) {
            (true, _) | (_, ForeignTypeSpec::Scalar(ForeignType::Void)) => self.codegen.types.void,
            (false, ForeignTypeSpec::Aggregate(id)) => {
                let ty = self
                    .codegen
                    .single_scalar_member(id)?
                    .ok_or(LlvmError::internal("a direct aggregate with no scalar"))?;
                self.codegen.foreign_c_type(ty)
            }
            (false, ForeignTypeSpec::Scalar(ty)) => self.codegen.foreign_c_type(ty),
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
        if !indirect_result && return_type != self.codegen.types.void {
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
