//! Lowering for the environment intrinsics.
//!
//! Each operation is one call to its `kira_rt_env_*` helper. The helper
//! consumes the string handle it is given — the native mirror of the VM
//! dropping the operand it popped — so nothing is freed here.
//!
//! `IsSet` comes back as an `i8`, the flag width every other runtime predicate
//! answers in, and is narrowed to `i1` so the rest of lowering sees the `Bool`
//! the type table says this is.

use llvm_sys::prelude::LLVMValueRef;

use kira_ir::ir::IrExprId;
use kira_runtime_abi::EnvOp;

use super::FunctionLowering;
use crate::LlvmError;

impl FunctionLowering<'_, '_> {
    /// Lowers one environment operation to its runtime call.
    pub(in crate::codegen) fn lower_env(
        &mut self,
        op: EnvOp,
        args: &[IrExprId],
    ) -> Result<LLVMValueRef, LlvmError> {
        let mut values = Vec::with_capacity(args.len());
        for &arg in args {
            values.push(self.lower_expr(arg)?);
        }
        let callee = self.codegen.runtime.env[usize::from(op.as_byte())];
        let call = self.call(callee, &mut values, c"env");
        match op {
            EnvOp::Text => Ok(call),
            EnvOp::IsSet => Ok(self.byte_to_bool(call)),
        }
    }
}
