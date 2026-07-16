//! Statement lowering: stores, returns, and the blocks `if`/`while` become.

use kira_ir::{IrExprId, IrStmt};
use kira_semantics_model::Type;
use llvm_sys::core::*;
use llvm_sys::prelude::*;

use super::FunctionLowering;
use crate::LlvmError;

impl FunctionLowering<'_, '_> {
    /// Lowers a statement list.
    pub(super) fn lower_block(&mut self, statements: &[IrStmt]) -> Result<(), LlvmError> {
        for statement in statements {
            // Everything after a `return` is unreachable; the VM simply never
            // executes it, and emitting into a terminated block is invalid IR.
            if self.block_is_terminated() {
                break;
            }
            self.lower_statement(statement)?;
        }
        Ok(())
    }

    /// Lowers one statement.
    fn lower_statement(&mut self, statement: &IrStmt) -> Result<(), LlvmError> {
        match statement {
            // `let` and `=` are the same store: the VM compiles both to
            // StoreLocal, which drops whatever the slot held.
            IrStmt::Let { local, init } => self.store_local(*local, *init),
            IrStmt::Assign { local, value } => self.store_local(*local, *value),
            IrStmt::Return { value } => {
                let returned = match value {
                    Some(expr) => Some(self.lower_expr(*expr)?),
                    None => None,
                };
                self.emit_return(returned)
            }
            IrStmt::Eval { expr } => {
                let value = self.lower_expr(*expr)?;
                // The VM's `Pop` drops the discarded value; only a string owns
                // anything to reclaim.
                if self.type_of(*expr) == Type::String {
                    self.free_string(value);
                }
                Ok(())
            }
            IrStmt::If {
                cond,
                then_body,
                else_body,
            } => self.lower_if(*cond, then_body, else_body),
            IrStmt::While { cond, body } => self.lower_while(*cond, body),
        }
    }

    /// Stores into a local slot, freeing the value it held.
    ///
    /// The new value is computed *before* the old one is freed, matching the
    /// VM's evaluate-then-StoreLocal order — which is what makes `s = s + "x"`
    /// work: the read clones `s` before the store frees the original.
    fn store_local(&mut self, slot: u32, expr: IrExprId) -> Result<(), LlvmError> {
        let value = self.lower_expr(expr)?;
        let ty = self.local_type(slot)?;
        let pointer = self.local_pointer(slot)?;

        if ty == Type::String {
            let llvm_type = self.codegen.llvm_type(ty)?;
            // SAFETY: `pointer` is this slot's alloca of `llvm_type`, and the
            // builder is positioned on a live block.
            let old = unsafe {
                LLVMBuildLoad2(self.codegen.builder, llvm_type, pointer, c"old".as_ptr())
            };
            // SAFETY: same slot and a matching value type.
            unsafe { LLVMBuildStore(self.codegen.builder, value, pointer) };
            self.free_string(old);
            return Ok(());
        }
        // SAFETY: `pointer` is this slot's alloca and `value` has its type.
        unsafe { LLVMBuildStore(self.codegen.builder, value, pointer) };
        Ok(())
    }

    /// Returns from the function, first reclaiming every string this frame owns.
    ///
    /// The VM drops a finished frame's locals after popping the result; because
    /// a local read clones, a returned string is never one of the slots being
    /// freed here.
    pub(super) fn emit_return(&mut self, value: Option<LLVMValueRef>) -> Result<(), LlvmError> {
        for slot in 0..self.function.locals.len() as u32 {
            if self.local_type(slot)? != Type::String {
                continue;
            }
            let pointer = self.local_pointer(slot)?;
            let llvm_type = self.codegen.types.ptr;
            // SAFETY: `pointer` is a live alloca holding a string handle.
            let held = unsafe {
                LLVMBuildLoad2(self.codegen.builder, llvm_type, pointer, c"drop".as_ptr())
            };
            self.free_string(held);
        }
        // SAFETY: the builder is positioned on an unterminated block, and
        // `value` matches the function's return type when present.
        unsafe {
            match value {
                Some(value) => LLVMBuildRet(self.codegen.builder, value),
                None => LLVMBuildRetVoid(self.codegen.builder),
            };
        }
        Ok(())
    }

    /// Lowers `if`/`else` into blocks.
    fn lower_if(
        &mut self,
        cond: IrExprId,
        then_body: &[IrStmt],
        else_body: &[IrStmt],
    ) -> Result<(), LlvmError> {
        let condition = self.lower_expr(cond)?;
        let function = self.current_function();
        let (then_block, else_block, merge_block) = (
            self.append_block(function, c"if.then"),
            self.append_block(function, c"if.else"),
            self.append_block(function, c"if.end"),
        );

        // SAFETY: all three blocks belong to the function being built.
        unsafe { LLVMBuildCondBr(self.codegen.builder, condition, then_block, else_block) };

        for (block, body) in [(then_block, then_body), (else_block, else_body)] {
            self.position_at(block);
            self.lower_block(body)?;
            if !self.block_is_terminated() {
                // SAFETY: the arm fell through; join the continuation.
                unsafe { LLVMBuildBr(self.codegen.builder, merge_block) };
            }
        }
        self.position_at(merge_block);
        Ok(())
    }

    /// Lowers a pre-tested loop into blocks.
    fn lower_while(&mut self, cond: IrExprId, body: &[IrStmt]) -> Result<(), LlvmError> {
        let function = self.current_function();
        let (test_block, body_block, exit_block) = (
            self.append_block(function, c"while.test"),
            self.append_block(function, c"while.body"),
            self.append_block(function, c"while.end"),
        );

        // SAFETY: every block belongs to the function being built, and the
        // condition is re-evaluated on each iteration as the VM does.
        unsafe { LLVMBuildBr(self.codegen.builder, test_block) };
        self.position_at(test_block);
        let condition = self.lower_expr(cond)?;
        // SAFETY: the test block is unterminated and `condition` is an `i1`.
        unsafe { LLVMBuildCondBr(self.codegen.builder, condition, body_block, exit_block) };

        self.position_at(body_block);
        self.lower_block(body)?;
        if !self.block_is_terminated() {
            // SAFETY: the body fell through; loop back to the test.
            unsafe { LLVMBuildBr(self.codegen.builder, test_block) };
        }
        self.position_at(exit_block);
        Ok(())
    }
}
