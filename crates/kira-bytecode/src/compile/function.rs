//! Statement, control-flow, place, and jump lowering.

use kira_ir::{IrAttempt, IrCallee, IrExpr, IrExprId, IrPlace, IrPlaceStep, IrStmt};

use crate::op::{FieldPath, Instruction, PathStep, PlacePath};

use super::{CompileError, FnCompiler, LoopFrame};

impl FnCompiler<'_> {
    pub(super) fn compile_body(&mut self, stmts: &[IrStmt]) -> Result<(), CompileError> {
        for stmt in stmts {
            self.compile_stmt(stmt)?;
        }
        Ok(())
    }

    fn compile_stmt(&mut self, stmt: &IrStmt) -> Result<(), CompileError> {
        match stmt {
            IrStmt::Let { local, init } => {
                self.compile_expr(*init)?;
                let slot = self.local_slot(*local)?;
                self.code.push(Instruction::StoreLocal(slot));
            }
            IrStmt::Assign { place, value } => {
                let slot = self.local_slot(place.local)?;
                if place.path.is_empty() {
                    self.compile_expr(*value)?;
                    self.code.push(Instruction::StoreLocal(slot));
                } else if place.is_all_fields() {
                    // The shape that predates arrays keeps the encoding it
                    // already had: a static field path needs nothing on the
                    // stack but the value.
                    self.compile_expr(*value)?;
                    let path = self.field_path(place)?;
                    self.code.push(Instruction::StoreField { slot, path });
                } else {
                    // Indices first, outermost to innermost, then the value.
                    // That order is the IR's contract — see `IrPlace` — and
                    // every backend follows it.
                    let path = self.compile_place_indices(place)?;
                    self.compile_expr(*value)?;
                    self.code.push(Instruction::StorePlace { slot, path });
                }
            }
            IrStmt::CellSet { slot, value } => {
                self.compile_expr(*value)?;
                let slot = self.local_slot(*slot)?;
                self.code.push(Instruction::CellSet(slot));
            }
            IrStmt::Return { value } => match value {
                Some(expr) => {
                    self.compile_expr(*expr)?;
                    self.code.push(Instruction::Return);
                }
                None => self.code.push(Instruction::ReturnVoid),
            },
            IrStmt::Eval { expr } => {
                self.compile_expr(*expr)?;
                // Discard the result of an expression evaluated for effect.
                self.code.push(Instruction::Pop);
            }
            IrStmt::If {
                cond,
                then_body,
                else_body,
            } => self.compile_if(*cond, then_body, else_body)?,
            IrStmt::Attempt { attempt } => self.compile_attempt(attempt)?,
            IrStmt::While { cond, body } => self.compile_while(*cond, body)?,
            IrStmt::Break => {
                let placeholder = self.emit_placeholder_jump();
                self.innermost_loop()?.break_jumps.push(placeholder);
            }
            IrStmt::Continue => {
                let target = self.innermost_loop()?.continue_target;
                self.code.push(Instruction::Jump(target));
            }
        }
        Ok(())
    }

    /// Compiles a linear `attempt` region with one common handler exit.
    ///
    /// A failed step runs its handler and jumps over every later `try`; a
    /// successful step falls through to the next setup. This is the bytecode
    /// form of the typed IR contract and keeps the handler path from executing
    /// later guarded calls accidentally.
    fn compile_attempt(&mut self, attempt: &IrAttempt) -> Result<(), CompileError> {
        let mut handler_exits = Vec::with_capacity(attempt.steps.len());
        for step in &attempt.steps {
            self.compile_body(&step.setup)?;
            self.compile_expr(step.error_condition)?;
            let to_success = self.emit_placeholder_jump_if_false();
            self.compile_body(&step.handler)?;
            handler_exits.push(self.emit_placeholder_jump());
            self.patch_to_here(to_success)?;
            self.compile_body(&step.success)?;
        }
        self.compile_body(&attempt.trailing)?;
        for exit in handler_exits {
            self.patch_to_here(exit)?;
        }
        Ok(())
    }

    /// The innermost enclosing loop's frame.
    ///
    /// Analysis rejects a `break`/`continue` outside a loop, so reaching this
    /// with an empty stack means the frontend let one through — a typed error
    /// rather than a panic, because a compiler never gets to end its caller.
    fn innermost_loop(&mut self) -> Result<&mut LoopFrame, CompileError> {
        let function = self.function_name;
        self.loops
            .last_mut()
            .ok_or_else(|| CompileError::JumpOutsideLoop {
                function: function.to_owned(),
            })
    }

    fn compile_if(
        &mut self,
        cond: IrExprId,
        then_body: &[IrStmt],
        else_body: &[IrStmt],
    ) -> Result<(), CompileError> {
        self.compile_expr(cond)?;
        let to_else = self.emit_placeholder_jump_if_false();
        self.compile_body(then_body)?;
        let to_end = self.emit_placeholder_jump();
        self.patch_to_here(to_else)?;
        self.compile_body(else_body)?;
        self.patch_to_here(to_end)
    }

    /// Compiles a loop, resolving any `break`/`continue` inside it.
    ///
    /// `continue` targets the condition test rather than the body, so a
    /// `continue` re-tests before iterating — the same thing falling off the
    /// end of the body does.
    fn compile_while(&mut self, cond: IrExprId, body: &[IrStmt]) -> Result<(), CompileError> {
        let loop_start = self.here();
        self.compile_expr(cond)?;
        let to_end = self.emit_placeholder_jump_if_false();
        self.loops.push(LoopFrame {
            continue_target: loop_start,
            break_jumps: Vec::new(),
        });
        let body_result = self.compile_body(body);
        let frame = self.loops.pop();
        body_result?;
        self.code.push(Instruction::Jump(loop_start));
        self.patch_to_here(to_end)?;
        // Every `break` lands after the loop's backward jump, which is exactly
        // where the failed condition test lands too.
        for placeholder in frame.map(|frame| frame.break_jumps).unwrap_or_default() {
            self.patch_to_here(placeholder)?;
        }
        Ok(())
    }

    /// Builds the static field path of an all-fields place.
    fn field_path(&self, place: &IrPlace) -> Result<FieldPath, CompileError> {
        let mut steps = Vec::with_capacity(place.path.len());
        for step in &place.path {
            match step {
                IrPlaceStep::Field(index) => steps.push(self.field_index(*index)?),
                // `is_all_fields` is what makes this unreachable; it is a typed
                // error rather than an unwrap because a compiler never gets to
                // end its caller's process.
                IrPlaceStep::Index(_) => {
                    return Err(CompileError::DynamicFieldPath {
                        function: self.function_name.to_owned(),
                    });
                }
            }
        }
        Ok(FieldPath::new(steps))
    }

    /// Emits a place's index expressions, outermost first, and returns the
    /// path describing the walk.
    ///
    /// The emission order *is* the runtime contract: the values land on the
    /// stack in path order, and the runtime pops them back off in reverse.
    pub(super) fn compile_place_indices(
        &mut self,
        place: &IrPlace,
    ) -> Result<PlacePath, CompileError> {
        let mut steps = Vec::with_capacity(place.path.len());
        for step in &place.path {
            match step {
                IrPlaceStep::Field(index) => steps.push(PathStep::Field(self.field_index(*index)?)),
                IrPlaceStep::Index(expr) => {
                    self.compile_expr(*expr)?;
                    steps.push(PathStep::Index);
                }
            }
        }
        Ok(PlacePath::new(steps))
    }

    /// Converts an IR local index to the wide bytecode slot representation.
    pub(super) fn local_slot(&self, slot: u32) -> Result<u64, CompileError> {
        Ok(u64::from(slot))
    }

    /// Whether parameter `position` of `callee` borrows its argument.
    ///
    /// A borrowed argument leaves the caller holding the value, so a local
    /// reaching one is read rather than taken. Every other callee — a builtin,
    /// a native half, a foreign symbol — takes what it is given.
    pub(super) fn argument_is_borrowed(&self, callee: IrCallee, position: usize) -> bool {
        let IrCallee::User(index) = callee else {
            return false;
        };
        let Some(function) = self.program.functions.get(index as usize) else {
            return false;
        };
        let slot = position as u32;
        function.by_pointer_params.contains(&slot) || function.by_reference_params.contains(&slot)
    }

    /// Compiles `expr` in a position that does not consume it.
    ///
    /// Only a local read differs: everywhere else the value is a temporary the
    /// position owns either way.
    pub(super) fn compile_borrowed_expr(&mut self, expr: IrExprId) -> Result<(), CompileError> {
        let IrExpr::Local(slot) = *self.program.expr(expr) else {
            return self.compile_expr(expr);
        };
        let slot = self.local_slot(slot)?;
        self.code.push(Instruction::LoadLocal(slot));
        Ok(())
    }

    /// Whether reading local `slot` takes it rather than copying it.
    ///
    /// A value that runs a user `Drop` is never copied, so a read moves it out
    /// — the same rule the native backend follows, and the reason the two
    /// engines agree on *when* a body runs. A borrowed parameter is excluded:
    /// it does not own the value, its body may read it more than once, and the
    /// copy it holds is a share whose release runs nothing.
    pub(super) fn local_is_taken(&self, slot: u32) -> bool {
        self.function
            .locals
            .get(slot as usize)
            .is_some_and(|&ty| self.program.types.runs_user_drop(ty))
            && !self.function.by_pointer_params.contains(&slot)
            && !self.function.by_reference_params.contains(&slot)
    }

    /// Converts an IR field index to the wide bytecode operand.
    pub(super) fn field_index(&self, index: u32) -> Result<u64, CompileError> {
        Ok(u64::from(index))
    }

    fn here(&self) -> u64 {
        self.code.len() as u64
    }

    pub(super) fn emit_placeholder_jump(&mut self) -> usize {
        let index = self.code.len();
        self.code.push(Instruction::Jump(0));
        index
    }

    pub(super) fn emit_placeholder_jump_if_false(&mut self) -> usize {
        let index = self.code.len();
        self.code.push(Instruction::JumpIfFalse(0));
        index
    }

    pub(super) fn patch_to_here(&mut self, placeholder: usize) -> Result<(), CompileError> {
        let target = self.code.len() as u64;
        match self.code.get_mut(placeholder) {
            Some(Instruction::Jump(slot)) | Some(Instruction::JumpIfFalse(slot)) => {
                *slot = target;
                Ok(())
            }
            _ => Err(CompileError::PatchedNonJump),
        }
    }
}
