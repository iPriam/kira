//! Lowering for the compiler intrinsics.
//!
//! Each operation is one call to its `kira_rt_compiler_*` helper. The helper
//! consumes the array handle it is given — the native mirror of the VM dropping
//! the operands it popped — so nothing is freed here.
//!
//! Both the argument and the result are `[String]`, so both calls carry the
//! element stride the target gives a string-handle element: the same number
//! `array_new` is handed everywhere else, so the runtime touches the slots
//! generated code does.

use llvm_sys::prelude::LLVMValueRef;

use kira_ir::ir::IrExprId;
use kira_runtime_abi::CompilerOp;
use kira_semantics_model::Type;

use super::FunctionLowering;
use crate::LlvmError;

impl FunctionLowering<'_, '_> {
    /// Lowers one compiler operation to its runtime call.
    pub(in crate::codegen) fn lower_compiler(
        &mut self,
        op: CompilerOp,
        args: &[IrExprId],
        ty: Type,
    ) -> Result<LLVMValueRef, LlvmError> {
        // Arguments evaluate left to right, as the VM pushes them.
        let mut values = Vec::with_capacity(args.len() + 1);
        for &arg in args {
            values.push(self.lower_expr(arg)?);
        }
        let element = self.codegen.element_of(ty)?;
        values.push(self.codegen.abi_size(element)?);

        let callee = self.codegen.runtime.compiler[usize::from(op.as_byte())];
        Ok(self.call(callee, &mut values, c"kc"))
    }
}
