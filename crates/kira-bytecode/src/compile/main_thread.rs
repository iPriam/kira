//! Bytecode lowering for host main-thread requests.

use kira_ir::{IrCallee, IrExprId};
use kira_runtime_abi::MainThreadOp;

use crate::op::Instruction;

use super::{CompileError, FnCompiler};

impl FnCompiler<'_> {
    /// Compiles a typed request after its arguments have been evaluated in
    /// source order.
    pub(super) fn compile_main_thread_call(
        &mut self,
        operation: MainThreadOp,
        function_index: u32,
        args: &[IrExprId],
    ) -> Result<(), CompileError> {
        let (param_count, has_writeback) = self
            .program
            .functions
            .get(function_index as usize)
            .map(|target| (target.param_count, !target.by_reference_params.is_empty()))
            .ok_or_else(|| CompileError::UnknownMainThreadTarget {
                function: self.function_name.to_owned(),
                target: function_index,
            })?;
        for (position, &arg) in args.iter().enumerate() {
            match self.argument_is_borrowed(IrCallee::User(function_index), position) {
                true => self.compile_borrowed_expr(arg)?,
                false => self.compile_expr(arg)?,
            }
        }
        if has_writeback {
            return Err(CompileError::MalformedMutCall {
                function: self.function_name.to_owned(),
            });
        }
        self.code.push(Instruction::MainThreadCall {
            operation,
            function: u64::from(function_index),
            args: u64::from(param_count),
        });
        Ok(())
    }

    /// Compiles the task-handle expression consumed by a main-thread join.
    pub(super) fn compile_main_thread_join(
        &mut self,
        handle: IrExprId,
    ) -> Result<(), CompileError> {
        self.compile_expr(handle)?;
        self.code.push(Instruction::MainThreadJoin);
        Ok(())
    }
}
