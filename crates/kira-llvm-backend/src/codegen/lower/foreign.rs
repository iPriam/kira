//! Foreign call lowering: marshalling a Kira call site to a generated adapter.
//!
//! The mirror of the VM's `CALL_FOREIGN` interpreter path. Arguments are packed
//! into a stack array of `BridgeValue`s tagged for the import's exact-width
//! signature, the generated adapter is called directly, and the checked result
//! is read back as a Kira value. A non-success status is a runtime trap, exactly
//! as the VM surfaces a `ForeignCallError` — there is no value to hand back.

use kira_ir::IrExprId;
use kira_runtime_abi::ForeignType;
use llvm_sys::LLVMIntPredicate;
use llvm_sys::core::*;
use llvm_sys::prelude::*;

use super::FunctionLowering;
use crate::LlvmError;

impl FunctionLowering<'_, '_> {
    /// Lowers a call to foreign import `index`.
    pub(super) fn lower_foreign_call(
        &mut self,
        index: u32,
        args: &[IrExprId],
    ) -> Result<LLVMValueRef, LlvmError> {
        let idx = index as usize;
        let import = self.codegen.program.foreign_imports[idx].import.clone();
        let signature = import.signature();
        let params: Vec<ForeignType> = signature.parameters().to_vec();
        let result = signature.result();
        let adapter = *self
            .codegen
            .foreign_adapters
            .get(idx)
            .ok_or(LlvmError::Unsupported(
                "a call to an undeclared foreign adapter",
            ))?;

        // Arguments evaluate left to right, as the VM pushes them.
        let mut values = Vec::with_capacity(args.len());
        for &argument in args {
            let ty = self.type_of(argument);
            values.push((self.lower_expr(argument)?, ty));
        }

        let types = self.codegen.types;
        let builder = self.codegen.builder;

        // SAFETY: every type and value belongs to this live module and the
        // builder is on a live block; `argv` is sized to hold exactly the
        // arguments written into it, and `out` addresses one bridge value.
        let (out, status) = unsafe {
            let count = LLVMConstInt(types.i64, values.len() as u64, 0);
            let argv =
                LLVMBuildArrayAlloca(builder, types.bridge_value, count, c"foreign.args".as_ptr());
            for (slot, ((value, ty), ft)) in values
                .iter()
                .copied()
                .zip(params.iter().copied())
                .enumerate()
            {
                let element = self.codegen.bridge_element_ptr(argv, slot as u64);
                self.codegen.write_foreign_arg(element, value, ty, ft)?;
            }
            let out = LLVMBuildAlloca(builder, types.bridge_value, c"foreign.out".as_ptr());
            let mut call_args = [argv, LLVMConstInt(types.i32, values.len() as u64, 0), out];
            let status = self
                .codegen
                .call_runtime(adapter, &mut call_args, c"foreign.status");
            (out, status)
        };

        // A non-success status is a runtime trap (an interior NUL, say): there is
        // no value to hand back, so native code reports it and exits, mirroring
        // the VM surfacing a `ForeignCallError`.
        let function = self.current_function();
        let ok = self.append_block(function, c"foreign.ok");
        let fail = self.append_block(function, c"foreign.fail");
        // SAFETY: the builder is on the call's block; `status` is the adapter's
        // `i32` return.
        unsafe {
            let zero = LLVMConstInt(types.i32, 0, 0);
            let failed = LLVMBuildICmp(
                builder,
                LLVMIntPredicate::LLVMIntNE,
                status,
                zero,
                c"foreign.failed".as_ptr(),
            );
            LLVMBuildCondBr(builder, failed, fail, ok);
        }
        self.position_at(fail);
        self.call(self.codegen.runtime.trap_foreign, &mut [status], c"");
        // SAFETY: `kira_rt_trap_foreign` never returns; the block is terminated.
        unsafe { LLVMBuildUnreachable(builder) };
        self.position_at(ok);

        self.codegen.read_foreign_result(out, result)
    }
}
