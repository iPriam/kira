//! Statement lowering: stores, returns, and the blocks `if`/`while` become.

use kira_ir::{IrExprId, IrPlace, IrPlaceStep, IrStmt};
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
            IrStmt::CellSet { slot, value } => self.lower_cell_set(*slot, *value),
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
        let ty = self.local_type(slot)?;
        if let Some(type_id) = self
            .function
            .native_state_locals
            .get(slot as usize)
            .copied()
            .flatten()
        {
            if let kira_ir::IrExpr::NativeRecover { raw, .. } = self.codegen.program.expr(expr) {
                let token = self.lower_native_recover_token(*raw, type_id)?;
                let pointer = self.local_pointer(slot)?;
                // SAFETY: a recovered-view local is an i64 token slot.
                unsafe { LLVMBuildStore(self.codegen.builder, token, pointer) };
                return Ok(());
            }
            let value = self.lower_expr(expr)?;
            return self.replace_native_state_local(slot, type_id, ty, value);
        }
        let value = self.lower_expr(expr)?;
        let pointer = self.local_pointer(slot)?;
        self.store_through(pointer, ty, value)
    }

    /// Stores into an assignment target, walking its path to the slot to write.
    ///
    /// The path is walked to the destination's address, so a nested write lands
    /// in place rather than rebuilding the enclosing value — matching the VM's
    /// place walk, which moves handles for the same reason.
    ///
    /// **Evaluation order** follows the IR contract: the walk evaluates the
    /// path's index expressions left to right, and only then is the value
    /// lowered — so `xs[next()] = next()` agrees across backends.
    fn store_place(&mut self, place: &IrPlace, expr: IrExprId) -> Result<(), LlvmError> {
        if let Some(type_id) = self
            .function
            .native_state_locals
            .get(place.local as usize)
            .copied()
            .flatten()
        {
            let root_ty = self.local_type(place.local)?;
            if place.path.is_empty() {
                let value = self.lower_expr(expr)?;
                return self.replace_native_state_local(place.local, type_id, root_ty, value);
            }
            // For a boxed state the walk starts at the state's own storage, so
            // the store lands in it and nothing is written back — that round
            // trip is what made writing one field cost the whole value.
            let (root, write_back) =
                self.recover_native_state_alloca(place.local, type_id, root_ty)?;
            let mut pointer = root;
            let mut ty = root_ty;
            for step in &place.path {
                (pointer, ty) = self.walk_place_step(pointer, ty, step)?;
            }
            let value = self.lower_expr(expr)?;
            self.store_through(pointer, ty, value)?;
            if write_back {
                return self.write_back_native_state(place.local, type_id, root_ty, root);
            }
            return Ok(());
        }
        if place.path.is_empty() {
            let value = self.lower_expr(expr)?;
            let ty = self.local_type(place.local)?;
            let pointer = self.local_pointer(place.local)?;
            return self.store_through(pointer, ty, value);
        }
        let (slot, ty) = self.walk_place(place.local, &place.path)?;
        let value = self.lower_expr(expr)?;
        self.store_through(slot, ty, value)
    }

    /// Walks `steps` from local `local`, leaving the address the last step
    /// reaches and that value's type.
    ///
    /// A struct is an inline aggregate, so a `Field` step is address arithmetic
    /// (a GEP). An array is an opaque handle stored in a slot, so an `Index`
    /// step *loads* that handle and asks the runtime for the element's address —
    /// which is where the two backends differ from a plain field chain, and
    /// where the bounds check lives.
    pub(super) fn walk_place(
        &mut self,
        local: u32,
        steps: &[IrPlaceStep],
    ) -> Result<(LLVMValueRef, Type), LlvmError> {
        let mut pointer = self.local_pointer(local)?;
        let mut ty = self.local_type(local)?;
        for step in steps {
            (pointer, ty) = self.walk_place_step(pointer, ty, step)?;
        }
        Ok((pointer, ty))
    }

    /// Walks one step: `pointer` is where the current value is stored, `ty` its
    /// type; returns the storage address of the value the step reaches and its
    /// type.
    pub(super) fn walk_place_step(
        &mut self,
        pointer: LLVMValueRef,
        ty: Type,
        step: &IrPlaceStep,
    ) -> Result<(LLVMValueRef, Type), LlvmError> {
        match step {
            IrPlaceStep::Field(index) => {
                let Type::Struct(id) = ty else {
                    return Err(LlvmError::Unsupported(
                        "a field of a value that is not a struct",
                    ));
                };
                let struct_type = self.codegen.llvm_type(ty)?;
                let name = c_string(&format!("field.{index}.ptr"));
                // SAFETY: `pointer` points at a value of `struct_type`, and
                // `index` came from that struct's own definition.
                let field_ptr = unsafe {
                    LLVMBuildStructGEP2(
                        self.codegen.builder,
                        struct_type,
                        pointer,
                        *index,
                        name.as_ptr(),
                    )
                };
                let field_ty = self
                    .codegen
                    .program
                    .types
                    .structs()
                    .get(id)
                    .and_then(|def| def.field(*index))
                    .map(|field| field.ty)
                    .ok_or(LlvmError::Unsupported("a field the struct never declared"))?;
                Ok((field_ptr, field_ty))
            }
            IrPlaceStep::Index(index) => {
                let element = self.codegen.element_of(ty)?;
                // A place walk exists to write at the end of it, and every
                // array it passes through is written *through* — so each one
                // takes its elements back from whatever it was sharing them
                // with. `pointer` is the slot holding the handle, which is what
                // the runtime needs: a split replaces the handle, and the slot
                // has to end up holding the fresh one. Doing this per step
                // rather than only at the end is what makes `rows[i].cells[j] =
                // v` land in this `rows` alone.
                let slot = self.element_slot_mut(pointer, *index, element)?;
                Ok((slot, element))
            }
        }
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
        // Which slots to release is not decided here. `kira_ir::mid` decides it
        // once, for both engines, from the same function this backend is
        // lowering — the skip conditions that used to sit inline (a pointer
        // parameter is the caller's storage, a callback-state local belongs to
        // a store outside the call, a scalar owns nothing) live there now, in
        // one place, rather than being re-derived per backend.
        let plan = kira_ir::mid::plan_function(
            self.function,
            &self.codegen.program.types,
            self.codegen.lending(),
        )
        .map_err(|error| LlvmError::Unsupported(mid_error_detail(error)))?;
        for &slot in plan.slots() {
            let ty = self.local_type(slot)?;
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

/// Renders a mid-stage failure as the detail an [`LlvmError::Unsupported`]
/// carries.
///
/// A release plan fails only on a contradiction inside one function — two
/// facts about a slot that cannot both hold — which is a compiler bug rather
/// than a program error. It is surfaced rather than swallowed because a
/// function whose plan could not be built would otherwise be lowered with no
/// releases at all, and leak silently.
fn mid_error_detail(error: kira_ir::mid::MidError) -> &'static str {
    match error {
        kira_ir::mid::MidError::ConflictingSlotRole { .. } => {
            "a local that is both a by-reference parameter and callback state"
        }
        kira_ir::mid::MidError::UnknownParameter { .. } => {
            "a by-reference parameter that names no local"
        }
    }
}
