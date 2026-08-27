//! Callback entries, which C reaches either as native functions or libffi
//! closures.
//!
//! Host and hybrid modules use a libffi closure because the callback address is
//! handed to a separately loaded C image. A Web module has one image, so
//! scalar-only callbacks are ordinary LLVM functions and callbacks with
//! by-value aggregates use a generated C entry. The C compiler owns the latter's
//! classification; LLVM only sees the entry's pointer-forwarded body.
//!
//! A host closure emits libffi's `(cif, result, arguments, user_data)` entry and
//! reads each argument out of the decoded `arguments` array. An aggregate is
//! already a pointer to its C-layout bytes there, which is exactly what the Kira
//! function's `@FFI.Pointer` parameter takes.

use kira_runtime_abi::{Execution, ForeignSignature, ForeignType, ForeignTypeSpec};
use kira_semantics_model::Type;
use llvm_sys::core::*;
use llvm_sys::prelude::*;

use super::{Callable, Codegen};
use crate::LlvmError;
use crate::shim::callback_needs_entry;
use crate::{callback_body_name, callback_name};

fn kira_type_of(spec: ForeignTypeSpec) -> Type {
    match spec {
        ForeignTypeSpec::Scalar(ty) => match ty {
            ForeignType::I8
            | ForeignType::U8
            | ForeignType::I16
            | ForeignType::U16
            | ForeignType::I32
            | ForeignType::U32
            | ForeignType::I64
            | ForeignType::U64 => Type::INT,
            ForeignType::F32 | ForeignType::F64 => Type::FLOAT,
            ForeignType::Bool => Type::Bool,
            ForeignType::RawPtr => Type::RawPtr,
            ForeignType::Void => Type::Void,
            ForeignType::CString => Type::String,
        },
        ForeignTypeSpec::Aggregate(_) => Type::RawPtr,
    }
}

fn result_of(spec: ForeignTypeSpec) -> Result<ForeignType, LlvmError> {
    match spec {
        ForeignTypeSpec::Scalar(ty) => Ok(ty),
        ForeignTypeSpec::Aggregate(_) => Err(LlvmError::internal(
            "an aggregate result in a callback signature",
        )),
    }
}

impl Codegen<'_> {
    /// Emits one closure entry per callback row.
    pub(super) fn emit_foreign_callbacks(&mut self) -> Result<(), LlvmError> {
        for index in 0..self.program.foreign_callbacks.len() {
            self.emit_foreign_callback(index)?;
        }
        Ok(())
    }

    /// Returns the address C stores for callback `index`.
    ///
    /// The closure is prepared on first use and kept for the process, so this
    /// yields one address per callback however often the value is materialized.
    pub(super) fn callback_thunk_address(
        &mut self,
        index: usize,
    ) -> Result<LLVMValueRef, LlvmError> {
        let entry = self
            .program
            .foreign_callbacks
            .get(index)
            .ok_or(LlvmError::internal("a callback not in the table"))?
            .clone();
        result_of(entry.signature().result())?;
        if self.calls_foreign_directly() {
            let surface = self.declare_callback_surface(index, entry.signature())?;
            // A Web module has one image and no loader or libffi closure to
            // manufacture an address. For an aggregate callback the generated
            // C entry owns the address; for a scalar callback this declaration
            // is the LLVM function itself.
            // SAFETY: `surface` is a function in this live module or a matching
            // external C entry, and the destination integer is the callback
            // value representation.
            return Ok(unsafe {
                LLVMBuildPtrToInt(
                    self.builder,
                    surface.value,
                    self.types.i64,
                    c"cb.direct.addr".as_ptr(),
                )
            });
        }
        let target = self.declare_callback_entry(index)?;
        let descriptor = self.callback_ffi_descriptor(index)?;
        let mut arguments = [descriptor, target.value];
        // SAFETY: both arguments are values in this live module and the runtime
        // declaration takes exactly two pointers.
        Ok(unsafe { self.call_runtime(self.runtime.ffi_closure, &mut arguments, c"cb.closure") })
    }

    /// Emits callback `index`'s entry after its declaration has been shared
    /// with the call sites that materialize its address.
    fn emit_foreign_callback(&mut self, index: usize) -> Result<(), LlvmError> {
        let entry = self.program.foreign_callbacks[index].clone();
        let signature = entry.signature();
        let result = result_of(signature.result())?;
        if self.calls_foreign_directly() {
            return self.emit_direct_callback(index, &entry, signature, result);
        }
        self.emit_closure_callback(index, &entry, signature, result)
    }

    /// Emits the libffi closure entry used by host and hybrid callbacks.
    fn emit_closure_callback(
        &mut self,
        index: usize,
        entry: &kira_runtime_abi::ForeignCallback,
        signature: &ForeignSignature,
        result: ForeignType,
    ) -> Result<(), LlvmError> {
        let target = self.declare_callback_entry(index)?;
        // SAFETY: the entry is a function just declared in this module and the
        // block belongs to its context.
        unsafe {
            let block =
                LLVMAppendBasicBlockInContext(self.context, target.value, c"entry".as_ptr());
            LLVMPositionBuilderAtEnd(self.builder, block);
        }

        let parameters = signature.parameters().to_vec();
        // SAFETY: the closure signature's third parameter is the decoded
        // argument array.
        let argument_array = unsafe { LLVMGetParam(target.value, 2) };
        let mut arguments = Vec::with_capacity(parameters.len());
        for (slot, spec) in parameters.iter().copied().enumerate() {
            arguments.push(self.read_closure_argument(argument_array, slot, spec)?);
        }

        let function = entry.function();
        let produced = if self.engine_of(function as usize) == Execution::Native {
            self.enter_kira_function(function, &arguments, result)?
        } else {
            self.enter_through_runtime(function, &parameters, &arguments, result)?
        };

        if let Some(value) = produced {
            // SAFETY: the closure signature's second parameter is the result
            // storage libffi sized for this signature.
            let storage = unsafe { LLVMGetParam(target.value, 1) };
            self.write_closure_result(storage, value, result);
        }
        // SAFETY: the builder is on the entry's only unterminated block.
        unsafe { LLVMBuildRetVoid(self.builder) };
        Ok(())
    }

    /// Emits the true-prototype callback body reached by a generated C entry.
    ///
    /// C owns classification of a by-value aggregate. Its entry takes that
    /// aggregate by value and forwards a pointer to this function, so LLVM
    /// only sees scalar C values and opaque pointers here. This is the direct
    /// Web path: the C entry's address is materialized without asking libffi to
    /// prepare a closure the Web runtime cannot provide.
    fn emit_direct_callback(
        &mut self,
        index: usize,
        entry: &kira_runtime_abi::ForeignCallback,
        signature: &ForeignSignature,
        result: ForeignType,
    ) -> Result<(), LlvmError> {
        let name = if callback_needs_entry(signature) {
            callback_body_name(index)
        } else {
            callback_name(index)
        };
        let target = self.declare_direct_callback(index, name, signature, result)?;
        // SAFETY: the target was declared in this module and the block belongs
        // to this module's live context.
        unsafe {
            let block =
                LLVMAppendBasicBlockInContext(self.context, target.value, c"entry".as_ptr());
            LLVMPositionBuilderAtEnd(self.builder, block);
        }

        let mut arguments = Vec::with_capacity(signature.parameters().len());
        for (slot, spec) in signature.parameters().iter().copied().enumerate() {
            // SAFETY: the declared function has exactly one parameter for each
            // signature position, and all values belong to this context.
            let value = unsafe { LLVMGetParam(target.value, slot as u32) };
            arguments.push(match spec {
                ForeignTypeSpec::Aggregate(_) => {
                    // The C entry deliberately passes the address of its own
                    // by-value copy. Kira callback functions take that address
                    // as the `RawPtr` representation used by field access.
                    // SAFETY: `value` is an opaque pointer parameter and the
                    // Kira representation is the i64 pointer word.
                    unsafe {
                        LLVMBuildPtrToInt(
                            self.builder,
                            value,
                            self.types.i64,
                            c"cb.aggregate.ptr".as_ptr(),
                        )
                    }
                }
                ForeignTypeSpec::Scalar(ForeignType::CString) => {
                    // A C string is copied into Kira-owned storage before the
                    // callback can retain or inspect it, just as in the libffi
                    // closure entry.
                    // SAFETY: `value` is the `const char *` C supplied for this
                    // callback position.
                    unsafe {
                        self.call_runtime(self.runtime.str_from_cstr, &mut [value], c"cb.str")
                    }
                }
                ForeignTypeSpec::Scalar(ty) => self.c_value_to_kira(value, ty)?,
            });
        }

        let function = entry.function();
        let produced = if self.engine_of(function as usize) == Execution::Native {
            self.enter_kira_function(function, &arguments, result)?
        } else {
            self.enter_through_runtime(function, signature.parameters(), &arguments, result)?
        };
        match produced {
            Some(value) => {
                // SAFETY: `value` was converted to the direct body's declared
                // C result type by `enter_*`.
                unsafe { LLVMBuildRet(self.builder, value) };
            }
            None => {
                // SAFETY: a void callback body has no result value.
                unsafe { LLVMBuildRetVoid(self.builder) };
            }
        }
        Ok(())
    }

    /// Declares one callback entry with libffi's closure signature.
    fn declare_callback_entry(&mut self, index: usize) -> Result<Callable, LlvmError> {
        // Host and hybrid modules expose the libffi closure entry under the
        // address name. A wasm module never calls this declaration for an
        // aggregate callback; its generated C surface owns that name and the
        // LLVM body uses `callback_body_name` instead.
        let name = super::ffi::c_string(&callback_name(index));
        let mut parameter_types = [
            self.types.ptr,
            self.types.ptr,
            self.types.ptr,
            self.types.ptr,
        ];
        // SAFETY: every type belongs to this module's context, and the
        // parameter array outlives the LLVM function-type call.
        let ty = unsafe {
            LLVMFunctionType(
                self.types.void,
                parameter_types.as_mut_ptr(),
                parameter_types.len() as u32,
                0,
            )
        };
        // SAFETY: the name belongs to this live module and LLVM returns the
        // existing declaration when one was made by a callback value.
        let value = unsafe {
            let existing = LLVMGetNamedFunction(self.module, name.as_ptr());
            if existing.is_null() {
                LLVMAddFunction(self.module, name.as_ptr(), ty)
            } else {
                existing
            }
        };
        Ok(Callable { ty, value })
    }

    /// Declares the external C entry whose address the Web callback stores.
    ///
    /// Its parameter list is the real C prototype: scalar positions retain
    /// their exact C type and aggregate positions are passed as pointers by
    /// the generated entry. The C translation unit defines this symbol.
    fn declare_callback_surface(
        &mut self,
        index: usize,
        signature: &ForeignSignature,
    ) -> Result<Callable, LlvmError> {
        let result = result_of(signature.result())?;
        self.declare_direct_callback(index, callback_name(index), signature, result)
    }

    /// Declares one direct callback function with scalar values and aggregate
    /// pointers in its LLVM-visible prototype.
    fn declare_direct_callback(
        &mut self,
        _index: usize,
        name: String,
        signature: &ForeignSignature,
        result: ForeignType,
    ) -> Result<Callable, LlvmError> {
        let mut parameters: Vec<LLVMTypeRef> = signature
            .parameters()
            .iter()
            .map(|spec| match spec {
                ForeignTypeSpec::Aggregate(_) => self.types.ptr,
                ForeignTypeSpec::Scalar(ty) => self.foreign_c_type(*ty),
            })
            .collect();
        // SAFETY: every type belongs to this module's context, and the
        // parameter vector outlives the function-type call.
        let ty = unsafe {
            LLVMFunctionType(
                self.foreign_c_type(result),
                parameters.as_mut_ptr(),
                parameters.len() as u32,
                0,
            )
        };
        let name = super::ffi::c_string(&name);
        // SAFETY: the name and type belong to this live module. Reusing an
        // existing declaration is what lets callback address materialization
        // precede body emission.
        let value = unsafe {
            let existing = LLVMGetNamedFunction(self.module, name.as_ptr());
            if existing.is_null() {
                LLVMAddFunction(self.module, name.as_ptr(), ty)
            } else {
                let found = LLVMGlobalGetValueType(existing);
                if found != ty {
                    return Err(LlvmError::SymbolCollision {
                        symbol: name.to_string_lossy().into_owned(),
                    });
                }
                existing
            }
        };
        Ok(Callable { ty, value })
    }

    /// Reads one decoded argument out of libffi's argument array.
    ///
    /// Each slot holds the address of that argument's value. An aggregate's
    /// address is what the Kira parameter takes; a scalar is loaded from it.
    fn read_closure_argument(
        &mut self,
        argument_array: LLVMValueRef,
        slot: usize,
        spec: ForeignTypeSpec,
    ) -> Result<LLVMValueRef, LlvmError> {
        let types = self.types;
        let builder = self.builder;
        // SAFETY: libffi supplies one readable storage pointer per declared
        // parameter, so slot `slot` is in bounds for this signature.
        let storage = unsafe {
            let mut offset = [LLVMConstInt(types.i64, slot as u64, 0)];
            let element = LLVMBuildInBoundsGEP2(
                builder,
                types.ptr,
                argument_array,
                offset.as_mut_ptr(),
                1,
                c"cb.arg.slot".as_ptr(),
            );
            LLVMBuildLoad2(builder, types.ptr, element, c"cb.arg.ptr".as_ptr())
        };
        match spec {
            ForeignTypeSpec::Aggregate(_) => {
                // SAFETY: the storage address is a pointer in this module and a
                // Kira `RawPtr` is its pointer-sized integer.
                Ok(unsafe {
                    LLVMBuildPtrToInt(builder, storage, types.i64, c"cb.aggregate.ptr".as_ptr())
                })
            }
            ForeignTypeSpec::Scalar(ty) => {
                let c_type = self.foreign_c_type(ty);
                // SAFETY: libffi wrote this argument's C value into `storage`.
                let value = unsafe { LLVMBuildLoad2(builder, c_type, storage, c"cb.arg".as_ptr()) };
                if ty != ForeignType::CString {
                    return self.c_value_to_kira(value, ty);
                }
                // SAFETY: `value` is the `const char*` C passed for this slot.
                Ok(unsafe {
                    self.call_runtime(self.runtime.str_from_cstr, &mut [value], c"cb.str")
                })
            }
        }
    }

    /// Writes one callback result into the storage libffi handed the closure.
    ///
    /// Libffi reads a whole `ffi_arg` word back for a narrow integral result, so
    /// an integer, `Bool`, or pointer result is stored as that word rather than
    /// as its own C width.
    fn write_closure_result(
        &mut self,
        storage: LLVMValueRef,
        value: LLVMValueRef,
        result: ForeignType,
    ) {
        let builder = self.builder;
        let types = self.types;
        match result {
            ForeignType::Void => {}
            ForeignType::F32 | ForeignType::F64 => {
                // SAFETY: `value` has this result's floating C type, which is
                // what libffi reads back for a floating result.
                unsafe { LLVMBuildStore(builder, value, storage) };
            }
            ForeignType::I8 | ForeignType::I16 | ForeignType::I32 | ForeignType::I64 => {
                // SAFETY: `value` is this result's signed C integer.
                unsafe {
                    let word = LLVMBuildSExt(builder, value, types.i64, c"cb.result".as_ptr());
                    LLVMBuildStore(builder, word, storage);
                }
            }
            ForeignType::Bool
            | ForeignType::U8
            | ForeignType::U16
            | ForeignType::U32
            | ForeignType::U64 => {
                // SAFETY: `value` is this result's unsigned C integer or `_Bool`.
                unsafe {
                    let word = LLVMBuildZExt(builder, value, types.i64, c"cb.result".as_ptr());
                    LLVMBuildStore(builder, word, storage);
                }
            }
            ForeignType::RawPtr | ForeignType::CString => {
                // SAFETY: `value` is this result's pointer.
                unsafe {
                    let word = LLVMBuildPtrToInt(builder, value, types.i64, c"cb.result".as_ptr());
                    LLVMBuildStore(builder, word, storage);
                }
            }
        }
    }

    /// Calls a native Kira callback body directly.
    fn enter_kira_function(
        &mut self,
        function: u32,
        arguments: &[LLVMValueRef],
        result: ForeignType,
    ) -> Result<Option<LLVMValueRef>, LlvmError> {
        let target = self
            .functions
            .get(function as usize)
            .copied()
            .flatten()
            .ok_or(LlvmError::internal(
                "a callback naming a function this module did not lower",
            ))?;
        let mut values = arguments.to_vec();
        // SAFETY: target is a lowered Kira function and values match its
        // parameter list after seam conversion.
        let produced = unsafe { self.call_runtime(target, &mut values, c"cb.call") };
        match result {
            ForeignType::Void => Ok(None),
            ty => Ok(Some(self.kira_value_to_c(produced, ty)?)),
        }
    }

    /// Calls a runtime-owned Kira callback through the hybrid invoker.
    fn enter_through_runtime(
        &mut self,
        function: u32,
        parameters: &[ForeignTypeSpec],
        arguments: &[LLVMValueRef],
        result: ForeignType,
    ) -> Result<Option<LLVMValueRef>, LlvmError> {
        let types = self.types;
        let builder = self.builder;
        // SAFETY: every type and value belongs to this live module, and the
        // builder is positioned on the entry's active block.
        let out = unsafe {
            let count = LLVMConstInt(types.i64, arguments.len() as u64, 0);
            let argv =
                LLVMBuildArrayAlloca(builder, types.bridge_value, count, c"cb.args".as_ptr());
            for (slot, (value, spec)) in arguments.iter().zip(parameters).enumerate() {
                let element = self.bridge_element_ptr(argv, slot as u64);
                self.write_bridge_value(element, *value, kira_type_of(*spec))?;
            }
            let out = LLVMBuildAlloca(builder, types.bridge_value, c"cb.out".as_ptr());
            let mut call_args = [
                LLVMConstInt(types.i32, u64::from(function), 0),
                argv,
                LLVMConstInt(types.i32, arguments.len() as u64, 0),
                out,
            ];
            self.call_runtime(self.runtime.call_runtime, &mut call_args, c"");
            out
        };
        if result == ForeignType::Void {
            return Ok(None);
        }
        let value = self.read_bridge_payload(out, kira_type_of(ForeignTypeSpec::Scalar(result)))?;
        Ok(Some(self.kira_value_to_c(value, result)?))
    }
}
