//! Statement lowering: stores, returns, and the blocks `if`/`while` become.

use kira_ir::{IrExprId, IrPlace, IrStmt};
use kira_semantics_model::Type;
use llvm_sys::core::*;
use llvm_sys::prelude::*;

use super::super::ffi::c_string;
use super::{FunctionLowering, LoopBlocks};
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
            // `let` and `=` to a bare local are the same store: the VM compiles
            // both to StoreLocal, which drops whatever the slot held.
            IrStmt::Let { local, init } => self.store_local(*local, *init),
            IrStmt::Assign { place, value } => self.store_place(place, *value),
            IrStmt::Return { value } => {
                let returned = match value {
                    Some(expr) => Some(self.lower_expr(*expr)?),
                    None => None,
                };
                self.emit_return(returned)
            }
            IrStmt::Eval { expr } => {
                let value = self.lower_expr(*expr)?;
                // The VM's `Pop` drops the discarded value.
                let ty = self.type_of(*expr);
                self.drop_value(value, ty)
            }
            IrStmt::If {
                cond,
                then_body,
                else_body,
            } => self.lower_if(*cond, then_body, else_body),
            IrStmt::While { cond, body } => self.lower_while(*cond, body),
            IrStmt::Break => {
                let exit = self.innermost_loop()?.exit;
                self.branch_to(exit);
                Ok(())
            }
            IrStmt::Continue => {
                let test = self.innermost_loop()?.test;
                self.branch_to(test);
                Ok(())
            }
        }
    }

    /// The innermost enclosing loop's blocks.
    ///
    /// Analysis rejects a `break`/`continue` outside a loop, so an empty stack
    /// means the frontend let one through — a typed error rather than a panic,
    /// because a backend never gets to end its caller's process.
    fn innermost_loop(&self) -> Result<&LoopBlocks, LlvmError> {
        self.loops.last().ok_or(LlvmError::JumpOutsideLoop)
    }

    /// Branches unconditionally to `target`, terminating the current block.
    ///
    /// Terminating is what makes statements after a `break` vanish:
    /// `lower_block` stops at a terminated block, so the unreachable tail is
    /// never emitted into invalid IR.
    fn branch_to(&mut self, target: LLVMBasicBlockRef) {
        // SAFETY: the builder sits on an unterminated block of the function
        // being built, and `target` is a block of that same function — it came
        // from the loop stack, which only ever holds blocks appended to it.
        unsafe { LLVMBuildBr(self.codegen.builder, target) };
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
        self.store_through(pointer, ty, value)
    }

    /// Stores into an assignment target, walking its field path.
    ///
    /// The path is walked with GEPs into the local's `alloca`, so a nested
    /// write lands in place rather than rebuilding the enclosing struct —
    /// matching the VM's `StoreField`, which walks handles for the same reason.
    fn store_place(&mut self, place: &IrPlace, expr: IrExprId) -> Result<(), LlvmError> {
        let value = self.lower_expr(expr)?;
        let mut pointer = self.local_pointer(place.local)?;
        let mut ty = self.local_type(place.local)?;
        for &index in &place.path {
            let Type::Struct(id) = ty else {
                return Err(LlvmError::Unsupported(
                    "a field of a value that is not a struct",
                ));
            };
            let struct_type = self.codegen.llvm_type(ty)?;
            let name = c_string(&format!("field.{index}.ptr"));
            // SAFETY: `pointer` points at a value of `struct_type`, and `index`
            // came from that struct's own definition, so it names a real field.
            pointer = unsafe {
                LLVMBuildStructGEP2(
                    self.codegen.builder,
                    struct_type,
                    pointer,
                    index,
                    name.as_ptr(),
                )
            };
            ty = self
                .codegen
                .program
                .structs
                .get(id)
                .and_then(|def| def.field(index))
                .map(|field| field.ty)
                .ok_or(LlvmError::Unsupported("a field the struct never declared"))?;
        }
        self.store_through(pointer, ty, value)
    }

    /// Stores `value` of type `ty` through `pointer`, dropping what was there.
    ///
    /// The new value is computed *before* the old one is freed, matching the
    /// VM's evaluate-then-store order — which is what makes `s = s + "x"` work:
    /// the read clones `s` before the store frees the original.
    fn store_through(
        &mut self,
        pointer: LLVMValueRef,
        ty: Type,
        value: LLVMValueRef,
    ) -> Result<(), LlvmError> {
        if self.owns_heap(ty) {
            let llvm_type = self.codegen.llvm_type(ty)?;
            // SAFETY: `pointer` points at a value of `llvm_type`, and the
            // builder is positioned on a live block.
            let old = unsafe {
                LLVMBuildLoad2(self.codegen.builder, llvm_type, pointer, c"old".as_ptr())
            };
            // SAFETY: same location and a matching value type.
            unsafe { LLVMBuildStore(self.codegen.builder, value, pointer) };
            return self.drop_value(old, ty);
        }
        // SAFETY: `pointer` points at a value of `ty` and `value` has its type.
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
            let ty = self.local_type(slot)?;
            if !self.owns_heap(ty) {
                continue;
            }
            let pointer = self.local_pointer(slot)?;
            let llvm_type = self.codegen.llvm_type(ty)?;
            // SAFETY: `pointer` is a live alloca holding a value of `llvm_type`.
            let held = unsafe {
                LLVMBuildLoad2(self.codegen.builder, llvm_type, pointer, c"drop".as_ptr())
            };
            self.drop_value(held, ty)?;
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
        self.loops.push(LoopBlocks {
            test: test_block,
            exit: exit_block,
        });
        let lowered = self.lower_block(body);
        self.loops.pop();
        lowered?;
        if !self.block_is_terminated() {
            // SAFETY: the body fell through; loop back to the test.
            unsafe { LLVMBuildBr(self.codegen.builder, test_block) };
        }
        self.position_at(exit_block);
        Ok(())
    }
}
