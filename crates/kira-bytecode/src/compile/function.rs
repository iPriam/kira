//! Statement, control-flow, place, and jump lowering.

use kira_ir::{IrExprId, IrPlace, IrPlaceStep, IrStmt};

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
        FieldPath::new(steps).map_err(|error| CompileError::FieldPathTooDeep {
            function: self.function_name.to_owned(),
            count: error.count,
        })
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
        PlacePath::new(steps).map_err(|error| CompileError::FieldPathTooDeep {
            function: self.function_name.to_owned(),
            count: error.count,
        })
    }

    /// Converts an IR local index to a `u16` slot, or fails typed.
    pub(super) fn local_slot(&self, slot: u32) -> Result<u16, CompileError> {
        u16::try_from(slot).map_err(|_| CompileError::LocalSlotOutOfRange {
            function: self.function_name.to_owned(),
            slot,
        })
    }

    /// Converts an IR field index to a `u16` operand, or fails typed.
    pub(super) fn field_index(&self, index: u32) -> Result<u16, CompileError> {
        u16::try_from(index).map_err(|_| CompileError::TooManyFields {
            function: self.function_name.to_owned(),
            count: index as usize,
        })
    }

    fn here(&self) -> u32 {
        self.code.len() as u32
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
        let target = self.code.len() as u32;
        match self.code.get_mut(placeholder) {
            Some(Instruction::Jump(slot)) | Some(Instruction::JumpIfFalse(slot)) => {
                *slot = target;
                Ok(())
            }
            _ => Err(CompileError::PatchedNonJump),
        }
    }
}
