//! Call lowering: `print`, direct calls within this half, and the crossing into
//! the VM half of a hybrid program.

use kira_ir::{IrCallee, IrExprId};
use kira_semantics_model::Type;
use llvm_sys::core::*;
use llvm_sys::prelude::*;

use super::FunctionLowering;
use crate::LlvmError;

impl FunctionLowering<'_, '_> {
    /// Lowers a call to `print` or a user function.
    pub(super) fn lower_call(
        &mut self,
        callee: IrCallee,
        args: &[IrExprId],
    ) -> Result<LLVMValueRef, LlvmError> {
        match callee {
            IrCallee::Print => {
                let argument = *args
                    .first()
                    .ok_or(LlvmError::Unsupported("a print with no argument"))?;
                let ty = self.type_of(argument);
                let mut value = self.lower_expr(argument)?;
                let helper = match ty {
                    // Analysis rejects printing a struct or an array — neither
                    // has a rendering the language pins — so a program that
                    // type-checked never reaches either arm.
                    Type::Array(_) => {
                        return Err(LlvmError::Unsupported("a print of an array"));
                    }
                    Type::Int => self.codegen.runtime.print_int,
                    Type::Float => self.codegen.runtime.print_float,
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
        }
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
