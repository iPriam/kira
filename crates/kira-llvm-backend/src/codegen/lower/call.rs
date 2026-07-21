//! Call lowering: `print`, direct calls within this half, and the crossing into
//! the VM half of a hybrid program.

use kira_ir::{IrCallee, IrExprId, IrPlace};
use kira_semantics_model::Type;
use llvm_sys::core::*;
use llvm_sys::prelude::*;

use super::FunctionLowering;
use crate::LlvmError;

impl FunctionLowering<'_, '_> {
    /// Lowers a call to `print` or a user function.
    ///
    /// `writeback` is `Some` only for a call to a method that mutates its
    /// receiver; then `args[0]` is the receiver and the place is where the
    /// mutation must land. The mutating case passes the receiver by pointer into
    /// that place, so the callee's writes to `self` are the caller's, and the
    /// call still yields the method's declared result.
    pub(super) fn lower_call(
        &mut self,
        callee: IrCallee,
        args: &[IrExprId],
        writeback: Option<&IrPlace>,
    ) -> Result<LLVMValueRef, LlvmError> {
        match callee {
            IrCallee::Print => {
                let argument = *args
                    .first()
                    .ok_or(LlvmError::Unsupported("a print with no argument"))?;
                let ty = self.type_of(argument);
                let mut value = self.lower_expr(argument)?;
                let helper = match ty {
                    // Analysis rejects printing a struct, an array, or an enum —
                    // none has a rendering the language pins — so a program that
                    // type-checked never reaches any of these arms.
                    Type::Array(_) => {
                        return Err(LlvmError::Unsupported("a print of an array"));
                    }
                    Type::Enum(_) => {
                        return Err(LlvmError::Unsupported("a print of an enum"));
                    }
                    Type::Int(_) => self.codegen.runtime.print_int,
                    Type::Float(_) => self.codegen.runtime.print_float,
                    Type::Bool => {
                        // Booleans are `i1` in registers but cross the C ABI as
                        // a byte.
                        // SAFETY: `value` is an `i1` and the builder is live.
                        value = unsafe {
                            LLVMBuildZExt(
                                self.codegen.builder,
                                value,
                                self.codegen.types.i8,
                                c"bool.byte".as_ptr(),
                            )
                        };
                        self.codegen.runtime.print_bool
                    }
                    // `print` consumes its string, so the helper frees it.
                    Type::String => self.codegen.runtime.print_str,
                    // Analysis rejects printing a struct — what it renders is
                    // not pinned by the language — so this is unreachable from
                    // a program that type-checked.
                    Type::Struct(_) => {
                        return Err(LlvmError::Unsupported("a print of a struct"));
                    }
                    // Analysis rejects `print` of a raw pointer (an opaque
                    // foreign word has no pinned rendering) and `CString` is
                    // seam-only, so neither reaches a type-checked program.
                    Type::RawPtr | Type::CString => {
                        return Err(LlvmError::Unsupported("a print of a raw pointer"));
                    }
                    Type::Void | Type::Error => {
                        return Err(LlvmError::Unsupported("printing a value with no type"));
                    }
                };
                Ok(self.call(helper, &mut [value], c""))
            }
            IrCallee::User(index) => {
                let target = *self
                    .codegen
                    .functions
                    .get(index as usize)
                    .ok_or(LlvmError::Unsupported("a call to an unknown function"))?;
                // A mutating method takes its receiver by reference: the caller
                // hands over a pointer into the receiver's place, so the callee's
                // writes to `self` land in the caller's storage.
                if let Some(place) = writeback {
                    return match target {
                        Some(_) => self.lower_mut_call(index, place, args),
                        // A struct receiver cannot cross the seam, so a mutating
                        // call is never to the VM half; the frontend and the
                        // bytecode compiler refuse it before here.
                        None => Err(LlvmError::Unsupported(
                            "a mutating method call across the hybrid seam",
                        )),
                    };
                }
                // Arguments evaluate left to right, as the VM pushes them.
                let mut values = Vec::with_capacity(args.len());
                for &argument in args {
                    values.push(self.lower_expr(argument)?);
                }
                match target {
                    // The callee is in this half: an ordinary direct call.
                    Some(target) => {
                        let returns_value = self.codegen.program.functions[index as usize]
                            .return_type
                            != Type::Void;
                        let name = if returns_value { c"call" } else { c"" };
                        Ok(self.call(target, &mut values, name))
                    }
                    // The callee runs on the VM: marshal and go through the
                    // bridge, which the host answers.
                    None => self.lower_runtime_call(index, args, &values),
                }
            }
            // A foreign C function: marshal the arguments to the import's
            // exact-width signature and invoke the generated adapter directly.
            IrCallee::Foreign(index) => self.lower_foreign_call(index, args),
        }
    }

    /// Lowers a call to a mutating method.
    ///
    /// The receiver — `args[0]` — is passed as a pointer into its place, walked
    /// exactly as `append` walks to the array it grows; the callee is compiled
    /// with a pointer parameter 0 (see `declare_function`) and mutates through
    /// it, so the write is the caller's. The remaining arguments are ordinary
    /// by-value arguments, evaluated left to right. The call still returns the
    /// method's declared result. Only ever a same-half call — a struct receiver
    /// cannot cross the seam — so `target` is resolved in this module.
    fn lower_mut_call(
        &mut self,
        index: u32,
        place: &IrPlace,
        args: &[IrExprId],
    ) -> Result<LLVMValueRef, LlvmError> {
        let target = self
            .codegen
            .functions
            .get(index as usize)
            .copied()
            .flatten()
            .ok_or(LlvmError::Unsupported(
                "a mutating call to a function not in this half",
            ))?;
        let (receiver_ptr, _ty) = self.walk_place(place.local, &place.path)?;
        let mut values = Vec::with_capacity(args.len());
        values.push(receiver_ptr);
        for &argument in args.iter().skip(1) {
            values.push(self.lower_expr(argument)?);
        }
        let returns_value =
            self.codegen.program.functions[index as usize].return_type != Type::Void;
        let name = if returns_value { c"call" } else { c"" };
        Ok(self.call(target, &mut values, name))
    }

    /// Calls a function that lives in the VM half, from native code.
    ///
    /// The mirror of the VM's `CallNative`: arguments are packed into a stack
    /// array of `BridgeValue`s, `kira_hybrid_call_runtime` hands them to the
    /// host's invoker, and the result is unpacked. The array is an `alloca`, so
    /// a crossing costs no heap allocation on this side either.
    fn lower_runtime_call(
        &mut self,
        index: u32,
        args: &[IrExprId],
        values: &[LLVMValueRef],
    ) -> Result<LLVMValueRef, LlvmError> {
        let builder = self.codegen.builder;
        let types = self.codegen.types;
        let result_type = self.codegen.program.functions[index as usize].return_type;

        // SAFETY: every type and value belongs to this live module and the
        // builder is on a live block; the argument array is sized to hold
        // exactly the arguments written into it.
        let out = unsafe {
            let count = LLVMConstInt(types.i64, values.len() as u64, 0);
            let argv =
                LLVMBuildArrayAlloca(builder, types.bridge_value, count, c"bridge.args".as_ptr());
            for (slot, (&value, &expr)) in values.iter().zip(args).enumerate() {
                let mut offset = [LLVMConstInt(types.i32, slot as u64, 0)];
                let element = LLVMBuildInBoundsGEP2(
                    builder,
                    types.bridge_value,
                    argv,
                    offset.as_mut_ptr(),
                    1,
                    c"bridge.arg".as_ptr(),
                );
                self.codegen
                    .write_bridge_value(element, value, self.type_of(expr))?;
            }

            let out = LLVMBuildAlloca(builder, types.bridge_value, c"bridge.out".as_ptr());
            let mut call_args = [
                LLVMConstInt(types.i32, u64::from(index), 0),
                argv,
                LLVMConstInt(types.i32, values.len() as u64, 0),
                out,
            ];
            self.codegen
                .call_runtime(self.codegen.runtime.call_runtime, &mut call_args, c"");
            out
        };
        self.codegen
            .read_bridge_payload(out, result_type)
            .or_else(|error| {
                // A `Void` callee returns nothing to read; anything else is a real
                // failure.
                if result_type == Type::Void {
                    // SAFETY: `i1 false` is a placeholder no caller of a Void call
                    // ever consumes; `Eval` discards it and nothing else can name it.
                    Ok(unsafe { LLVMConstInt(types.i1, 0, 0) })
                } else {
                    Err(error)
                }
            })
    }
}
