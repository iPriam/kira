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
//!
//! # A struct C passes by value
//!
//! One kind of parameter this module cannot present to C: a struct passed by
//! value, whose ABI is the target C compiler's to decide and never this
//! backend's. For such a callback the address C holds is a generated C entry
//! (see [`crate::shim`]) which takes the struct by value and calls the thunk
//! here with its address — so from this file's side the parameter is simply a
//! pointer, and the Kira function it enters declares an `@FFI.Pointer` to the
//! struct and reads members through it.

use kira_runtime_abi::{Execution, ForeignType, ForeignTypeSpec};
use kira_semantics_model::Type;
use llvm_sys::core::*;
use llvm_sys::prelude::*;

use super::{Callable, Codegen};
use crate::LlvmError;
use crate::callback_thunk_symbol;

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

/// The scalar a callback *result* carries, or a typed refusal.
///
/// The frontend admits no aggregate result into a callback signature — there is
/// nothing on the Kira side to build the C bytes out of — so this cannot fire
/// for a program that type-checked; it is the backend saying so rather than
/// assuming it.
fn result_of(spec: ForeignTypeSpec) -> Result<ForeignType, LlvmError> {
    match spec {
        ForeignTypeSpec::Scalar(ty) => Ok(ty),
        ForeignTypeSpec::Aggregate(_) => Err(LlvmError::Unsupported(
            "an aggregate result in a callback signature",
        )),
    }
}

/// The type one callback parameter reaches this thunk as.
///
/// A scalar is itself. A struct C passes by value reaches here as a pointer to
/// the entry's own copy: the generated C entry is what C actually calls, and it
/// is the C compiler there — not this backend — that decides how the struct
/// arrived.
fn thunk_param(spec: ForeignTypeSpec) -> ForeignType {
    match spec {
        ForeignTypeSpec::Scalar(ty) => ty,
        ForeignTypeSpec::Aggregate(_) => ForeignType::RawPtr,
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

    /// The address C holds for callback `index`, as a `RawPtr` value.
    ///
    /// Declared rather than looked up: for a scalar-only callback the thunk is
    /// emitted by this same module after the function bodies that name it, and
    /// LLVM resolves the two by name. For one C enters with a struct by value
    /// the definition is in the generated shim's object, which the link resolves
    /// — and its prototype is never spelled here, because spelling it would mean
    /// classifying the struct.
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
        let held = if crate::shim::callback_needs_entry(entry.signature()) {
            self.declare_shim_callback_entry(index)
        } else {
            let params: Vec<ForeignType> = entry
                .signature()
                .parameters()
                .iter()
                .copied()
                .map(thunk_param)
                .collect();
            let result = result_of(entry.signature().result())?;
            self.declare_callback_thunk(index, entry.signature(), &params, result)
                .value
        };
        let types = self.types;
        // SAFETY: `held` is a function in this live module, and a `RawPtr` is an
        // `i64` here.
        Ok(unsafe { LLVMBuildPtrToInt(self.builder, held, types.i64, c"cb.addr".as_ptr()) })
    }

    /// Declares the shim's C entry for callback `index`, for its address alone.
    ///
    /// Typed `void (void)` on purpose: nothing in this module calls it, and its
    /// real prototype takes a struct by value — which is precisely the shape
    /// this backend must not commit to. An address does not depend on a
    /// prototype, so declaring the least is declaring the truth.
    fn declare_shim_callback_entry(&mut self, index: usize) -> LLVMValueRef {
        let name = super::ffi::c_string(&crate::callback_name(index));
        // SAFETY: the type belongs to this module's context, and the name
        // outlives both calls below.
        unsafe {
            let existing = LLVMGetNamedFunction(self.module, name.as_ptr());
            if !existing.is_null() {
                return existing;
            }
            let ty = LLVMFunctionType(self.types.void, std::ptr::null_mut(), 0, 0);
            LLVMAddFunction(self.module, name.as_ptr(), ty)
        }
    }

    /// Emits the entry thunk for callback `index`.
    fn emit_foreign_callback(&mut self, index: usize) -> Result<(), LlvmError> {
        let entry = self.program.foreign_callbacks[index].clone();
        let signature = entry.signature();
        let params: Vec<ForeignType> = signature
            .parameters()
            .iter()
            .copied()
            .map(thunk_param)
            .collect();
        let result = result_of(signature.result())?;
        let function = entry.function();

        let thunk = self.declare_callback_thunk(index, signature, &params, result);
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

    /// Declares the thunk for callback `index` with the signature it is entered
    /// with here — every by-value struct already reduced to a pointer.
    ///
    /// The name is [`callback_thunk_symbol`]: the address C holds for a
    /// scalar-only callback, and the body behind the shim's entry otherwise.
    fn declare_callback_thunk(
        &mut self,
        index: usize,
        signature: &kira_runtime_abi::ForeignSignature,
        params: &[ForeignType],
        result: ForeignType,
    ) -> Callable {
        let name = super::ffi::c_string(&callback_thunk_symbol(index, signature));
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
