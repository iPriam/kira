//! Generated callback entry thunks: the address C holds for a Kira function.
//!
//! A `@FFI.Callback` value is a C function pointer, and C calls through it with
//! no knowledge of Kira at all. So for each callback entry the frontend
//! recorded, the backend emits one `extern "C"` function with exactly the
//! declared C signature — `kira_ffi_callback_<i>` — and the value of the
//! callback is that function's address.
//!
//! # Two bodies, one signature
//!
//! What the thunk does inside depends on where the target function's body is in
//! *this* module:
//!
//! * **Native here.** The thunk converts each C argument to its Kira value,
//!   calls the lowered function directly, and converts the result back. An
//!   executable's callbacks are all this shape, and so are a hybrid half's
//!   `@Native` targets: the call costs one conversion per argument and nothing
//!   else.
//! * **Not native here.** The body is bytecode the VM runs, which this module
//!   cannot call. The thunk marshals into `BridgeValue`s and goes through
//!   `kira_hybrid_call_runtime`, the same door a `@Native` function already uses
//!   to call a `@Runtime` one — so the adapter sidecar of a VM run and the
//!   native half of a hybrid program reach the interpreter through one path
//!   rather than two.
//!
//! Both are the same C function to the caller, which is the point: whether the
//! Kira function behind a callback is interpreted or compiled is not something a
//! C library can be asked to know.

use kira_runtime_abi::{Execution, ForeignType, ForeignTypeSpec};
use kira_semantics_model::Type;
use llvm_sys::core::*;
use llvm_sys::prelude::*;

use super::{Callable, Codegen};
use crate::LlvmError;
use crate::callback_name;

/// The Kira value type a seam scalar arrives as.
///
/// The widths collapse: every integer width is a Kira `Int` once converted, as
/// it is in the VM, and the seam type is what fixed the C width on the way in.
fn kira_type_of(ft: ForeignType) -> Result<Type, LlvmError> {
    Ok(match ft {
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
        // C hands the thunk a `const char*` it keeps; the thunk copies the
        // bytes, so what the Kira function receives is an owned `String`.
        ForeignType::CString => Type::String,
    })
}

/// The one scalar a callback position carries, or a typed refusal.
///
/// The frontend admits no aggregate into a callback signature, so this cannot
/// fire for a program that type-checked; it is the backend saying so rather than
/// assuming it.
fn scalar_of(spec: ForeignTypeSpec) -> Result<ForeignType, LlvmError> {
    match spec {
        ForeignTypeSpec::Scalar(ty) => Ok(ty),
        ForeignTypeSpec::Aggregate(_) => Err(LlvmError::Unsupported(
            "an aggregate in a callback signature",
        )),
    }
}

impl Codegen<'_> {
    /// Emits one entry thunk per callback the program records.
    pub(super) fn emit_foreign_callbacks(&mut self) -> Result<(), LlvmError> {
        for index in 0..self.program.foreign_callbacks.len() {
            self.emit_foreign_callback(index)?;
        }
        Ok(())
    }

    /// The address of callback `index`'s entry thunk, as a `RawPtr` value.
    ///
    /// Declared rather than looked up: the thunk is emitted by this same module
    /// after the function bodies that name it, and LLVM resolves the two by
    /// name.
    pub(super) fn callback_thunk_address(
        &mut self,
        index: usize,
    ) -> Result<LLVMValueRef, LlvmError> {
        let entry = self
            .program
            .foreign_callbacks
            .get(index)
            .ok_or(LlvmError::Unsupported("a callback not in the table"))?
            .clone();
        let params: Vec<ForeignType> = entry
            .signature()
            .parameters()
            .iter()
            .copied()
            .map(scalar_of)
            .collect::<Result<_, _>>()?;
        let result = scalar_of(entry.signature().result())?;
        let thunk = self.declare_callback_thunk(index, &params, result);
        let types = self.types;
        // SAFETY: `thunk.value` is a function in this live module, and a
        // `RawPtr` is an `i64` here.
        Ok(unsafe { LLVMBuildPtrToInt(self.builder, thunk.value, types.i64, c"cb.addr".as_ptr()) })
    }

    /// Emits the entry thunk for callback `index`.
    fn emit_foreign_callback(&mut self, index: usize) -> Result<(), LlvmError> {
        let entry = self.program.foreign_callbacks[index].clone();
        let signature = entry.signature();
        let params: Vec<ForeignType> = signature
            .parameters()
            .iter()
            .copied()
            .map(scalar_of)
            .collect::<Result<_, _>>()?;
        let result = scalar_of(signature.result())?;
        let function = entry.function();

        let thunk = self.declare_callback_thunk(index, &params, result);
        let builder = self.builder;
        // SAFETY: `thunk.value` is a function just declared in this live module,
        // and the block belongs to its context.
        unsafe {
            let block = LLVMAppendBasicBlockInContext(self.context, thunk.value, c"entry".as_ptr());
            LLVMPositionBuilderAtEnd(builder, block);
        }

        // SAFETY: the parameters exist — the signature was built with them — and
        // the builder is positioned on the thunk's only block.
        let arguments: Vec<LLVMValueRef> = (0..params.len())
            .map(|slot| unsafe { LLVMGetParam(thunk.value, slot as u32) })
            .collect();

        // Whether this module holds the function's body decides which door the
        // thunk takes; both present the same C function to the caller.
        let produced = if self.engine_of(function as usize) == Execution::Native {
            self.enter_kira_function(function, &params, &arguments, result)?
        } else {
            self.enter_through_runtime(function, &params, &arguments, result)?
        };

        // SAFETY: the builder is on the thunk's unterminated block, and
        // `produced` (when present) has this result's C type.
        unsafe {
            match produced {
                None => LLVMBuildRetVoid(builder),
                Some(value) => LLVMBuildRet(builder, value),
            };
        }
        Ok(())
    }

    /// Declares `kira_ffi_callback_<index>` with its exact C signature.
    fn declare_callback_thunk(
        &mut self,
        index: usize,
        params: &[ForeignType],
        result: ForeignType,
    ) -> Callable {
        let name = super::ffi::c_string(&callback_name(index));
        let mut param_types: Vec<LLVMTypeRef> =
            params.iter().map(|ty| self.foreign_c_type(*ty)).collect();
        let result_type = self.foreign_c_type(result);
        // SAFETY: every type belongs to this module's context, and `param_types`
        // outlives the type call.
        unsafe {
            let ty = LLVMFunctionType(
                result_type,
                param_types.as_mut_ptr(),
                param_types.len() as u32,
                0,
            );
            // A call site declares the thunk before the emission pass defines
            // it, so this has to return the *same* function both times: adding
            // one twice would give the second a suffixed name, and the address a
            // value carries would not be the address C is handed.
            let existing = LLVMGetNamedFunction(self.module, name.as_ptr());
            let value = if existing.is_null() {
                LLVMAddFunction(self.module, name.as_ptr(), ty)
            } else {
                existing
            };
            Callable { ty, value }
        }
    }

    /// Converts one C argument the thunk was entered with into the Kira value
    /// the function expects.
    ///
    /// Everything but a `CString` is the shared scalar conversion. A `CString`
    /// is the one position where the thunk *allocates*: C hands over a pointer
    /// into storage it keeps, and the Kira function is entered with an owned
    /// `String` copied from it. That copy has to happen here rather than
    /// anywhere later, because a callback's argument is only guaranteed valid
    /// for the length of the call C made.
    fn callback_argument_to_kira(
        &self,
        value: LLVMValueRef,
        ty: ForeignType,
    ) -> Result<LLVMValueRef, LlvmError> {
        if ty != ForeignType::CString {
            return self.c_value_to_kira(value, ty);
        }
        // SAFETY: `value` is the thunk's `ptr` parameter on its live entry
        // block, which is exactly what this helper takes.
        Ok(unsafe { self.call_runtime(self.runtime.str_from_cstr, &mut [value], c"cb.str") })
    }

    /// Calls the lowered Kira function directly, converting across the C types.
    ///
    /// `None` when the callback returns nothing.
    fn enter_kira_function(
        &mut self,
        function: u32,
        params: &[ForeignType],
        arguments: &[LLVMValueRef],
        result: ForeignType,
    ) -> Result<Option<LLVMValueRef>, LlvmError> {
        let target = self
            .functions
            .get(function as usize)
            .copied()
            .flatten()
            .ok_or(LlvmError::Unsupported(
                "a callback naming a function this module did not lower",
            ))?;
        let mut values = Vec::with_capacity(arguments.len());
        for (value, ty) in arguments.iter().zip(params) {
            values.push(self.callback_argument_to_kira(*value, *ty)?);
        }
        // SAFETY: `target` is a lowered Kira function of this module and
        // `values` matches its parameter list, converted one for one.
        let produced = unsafe { self.call_runtime(target, &mut values, c"cb.call") };
        match result {
            ForeignType::Void => Ok(None),
            ty => Ok(Some(self.kira_value_to_c(produced, ty)?)),
        }
    }

    /// Calls the function through `kira_hybrid_call_runtime`, which reaches the
    /// interpreter.
    fn enter_through_runtime(
        &mut self,
        function: u32,
        params: &[ForeignType],
        arguments: &[LLVMValueRef],
        result: ForeignType,
    ) -> Result<Option<LLVMValueRef>, LlvmError> {
        let types = self.types;
        let builder = self.builder;
        // SAFETY: every type and value belongs to this live module, the builder
        // is on the thunk's block, and the array is sized to hold exactly the
        // arguments written into it.
        let out = unsafe {
            let count = LLVMConstInt(types.i64, arguments.len() as u64, 0);
            let argv =
                LLVMBuildArrayAlloca(builder, types.bridge_value, count, c"cb.args".as_ptr());
            for (slot, (value, ty)) in arguments.iter().zip(params).enumerate() {
                let kira = self.callback_argument_to_kira(*value, *ty)?;
                let mut offset = [LLVMConstInt(types.i32, slot as u64, 0)];
                let element = LLVMBuildInBoundsGEP2(
                    builder,
                    types.bridge_value,
                    argv,
                    offset.as_mut_ptr(),
                    1,
                    c"cb.arg".as_ptr(),
                );
                self.write_bridge_value(element, kira, kira_type_of(*ty)?)?;
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
        let value = self.read_bridge_payload(out, kira_type_of(result)?)?;
        Ok(Some(self.kira_value_to_c(value, result)?))
    }
}
