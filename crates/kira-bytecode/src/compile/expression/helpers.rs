use super::*;

pub(super) struct SelectBranch<'a> {
    pub(super) setup: &'a [kira_ir::IrStmt],
    pub(super) value: IrExprId,
    pub(super) cleanup: &'a [u32],
}

pub(super) struct SelectInput<'a> {
    pub(super) cond: IrExprId,
    pub(super) then_branch: SelectBranch<'a>,
    pub(super) otherwise_branch: SelectBranch<'a>,
}

impl FnCompiler<'_> {
    /// Compiles a call whose callee writes through one or more of its
    /// parameters.
    ///
    /// The arguments are pushed exactly as an ordinary call pushes them; each
    /// target's place index expressions follow, targets in order, per the place
    /// convention — so the runtime pops the indices off the top before the
    /// arguments. When the whole of it is the receiver (callee slot 0), the
    /// instruction is the original [`Instruction::CallMut`], which encodes that
    /// one case in fewer bytes; anything else is
    /// [`Instruction::CallWriteback`].
    pub(super) fn compile_writeback_call(
        &mut self,
        callee: IrCallee,
        args: &[IrExprId],
        writebacks: &[IrWriteback],
    ) -> Result<(), CompileError> {
        // Only a user function ever writes back: `print` and a foreign function
        // have no Kira parameter slot to move out of.
        let IrCallee::User(index) = callee else {
            return Err(CompileError::MalformedMutCall {
                function: self.function_name.to_owned(),
            });
        };
        let native = self
            .engines
            .get(index as usize)
            .is_some_and(|engine| *engine == Execution::Native);
        for (position, &arg) in args.iter().enumerate() {
            let written = writebacks
                .iter()
                .any(|writeback| writeback.param as usize == position);
            match written {
                true => self.compile_writeback_argument(arg)?,
                false => self.compile_expr(arg)?,
            }
        }
        let mut targets = Vec::with_capacity(writebacks.len());
        for writeback in writebacks {
            let slot = self.local_slot(writeback.place.local)?;
            let path = self.compile_place_indices(&writeback.place)?;
            let param = u64::from(writeback.param);
            targets.push(WritebackTarget { param, slot, path });
        }
        // A seam crossing takes the general form even for a single slot-0
        // target: `CallMut`'s compactness buys nothing against a call that is
        // already marshalling a value into another engine's representation, and
        // one shape means one protocol to keep in step with the trampoline.
        if native {
            self.code.push(Instruction::CallNativeWriteback {
                func: index,
                targets,
            });
            return Ok(());
        }
        match targets.as_slice() {
            [target] if target.param == 0 => self.code.push(Instruction::CallMut {
                func: u64::from(index),
                slot: target.slot,
                path: target.path.clone(),
            }),
            _ => self.code.push(Instruction::CallWriteback {
                func: u64::from(index),
                targets,
            }),
        }
        Ok(())
    }

    /// Compiles the argument of a written-through parameter.
    ///
    /// A local whose type runs a user `Drop` is *taken* here even when
    /// [`Self::local_is_taken`] would leave it alone, because the call writes
    /// the value back into the same storage: a store into a slot that still
    /// holds the old value releases it, which would run the body on the value
    /// the call is about to hand back. Emptying the slot first is what makes
    /// the write-back a return of the value rather than a replacement of it.
    pub(super) fn compile_writeback_argument(&mut self, arg: IrExprId) -> Result<(), CompileError> {
        let IrExpr::Local(slot) = *self.program.expr(arg) else {
            return self.compile_expr(arg);
        };
        let runs_drop = self
            .function
            .locals
            .get(slot as usize)
            .is_some_and(|&ty| self.program.types.takes_on_read(ty));
        if !runs_drop {
            return self.compile_expr(arg);
        }
        let slot = self.local_slot(slot)?;
        self.code.push(Instruction::TakeLocal(slot));
        Ok(())
    }

    /// Whether the function at `index` takes any parameter by reference, and so
    /// requires its call sites to carry writebacks.
    pub(super) fn function_writes_back(&self, index: u32) -> bool {
        self.program
            .functions
            .get(index as usize)
            .is_some_and(|function| !function.by_reference_params.is_empty())
    }

    pub(super) fn compile_binary(
        &mut self,
        op: IrBinOp,
        lhs: IrExprId,
        rhs: IrExprId,
        ty: Type,
    ) -> Result<(), CompileError> {
        match op {
            IrBinOp::And => self.compile_and(lhs, rhs),
            IrBinOp::Or => self.compile_or(lhs, rhs),
            other => {
                self.compile_expr(lhs)?;
                self.compile_expr(rhs)?;
                self.compile_int_operator(other, ty)
            }
        }
    }

    /// `a && b`: evaluate `b` only when `a` is true.
    pub(super) fn compile_and(&mut self, lhs: IrExprId, rhs: IrExprId) -> Result<(), CompileError> {
        self.compile_expr(lhs)?;
        let to_false = self.emit_placeholder_jump_if_false();
        self.compile_expr(rhs)?;
        let to_end = self.emit_placeholder_jump();
        self.patch_to_here(to_false)?;
        self.code.push(Instruction::ConstBool(false));
        self.patch_to_here(to_end)
    }

    /// `c ? a : b`: evaluate exactly one branch.
    ///
    /// The same jump-and-patch shape as `&&`/`||`, which is why a conditional
    /// expression needs no opcode of its own: the branch already exists, and
    /// both branches leave one value on the stack, so the join is implicit.
    pub(super) fn compile_select(&mut self, select: SelectInput<'_>) -> Result<(), CompileError> {
        let SelectInput {
            cond,
            then_branch,
            otherwise_branch,
        } = select;
        self.compile_expr(cond)?;
        let to_else = self.emit_placeholder_jump_if_false();
        self.compile_body(then_branch.setup)?;
        self.compile_expr(then_branch.value)?;
        self.compile_branch_cleanup(then_branch.cleanup)?;
        let to_end = self.emit_placeholder_jump();
        self.patch_to_here(to_else)?;
        self.compile_body(otherwise_branch.setup)?;
        self.compile_expr(otherwise_branch.value)?;
        self.compile_branch_cleanup(otherwise_branch.cleanup)?;
        self.patch_to_here(to_end)
    }

    /// Releases locals whose lexical scope belongs to one expression branch.
    /// The branch value is already on the operand stack, so taking and popping
    /// the locals cannot destroy the result copy that is about to cross the
    /// join.
    fn compile_branch_cleanup(&mut self, locals: &[u32]) -> Result<(), CompileError> {
        for &local in locals {
            let slot = self.local_slot(local)?;
            self.code.push(Instruction::TakeLocal(slot));
            self.code.push(Instruction::Pop);
        }
        Ok(())
    }

    /// `a || b`: evaluate `b` only when `a` is false.
    pub(super) fn compile_or(&mut self, lhs: IrExprId, rhs: IrExprId) -> Result<(), CompileError> {
        self.compile_expr(lhs)?;
        let to_rhs = self.emit_placeholder_jump_if_false();
        self.code.push(Instruction::ConstBool(true));
        let to_end = self.emit_placeholder_jump();
        self.patch_to_here(to_rhs)?;
        self.compile_expr(rhs)?;
        self.patch_to_here(to_end)
    }

    /// The byte offset of one member of a C-layout aggregate.
    pub(super) fn foreign_member_offset(
        &self,
        aggregate: kira_runtime_abi::ForeignAggregateId,
        member: u32,
    ) -> Result<u32, CompileError> {
        self.program
            .foreign_aggregates
            .member_offsets_of(aggregate, ForeignPointerWidth::HOST)
            .ok()
            .and_then(|offsets| offsets.get(member as usize).copied())
            .ok_or_else(|| CompileError::ForeignMemberMissing {
                function: self.function_name.to_owned(),
                member,
            })
    }

    /// The seam type of one member of a C-layout aggregate.
    ///
    /// Only a scalar member is loadable; semantics refuses a nested aggregate or
    /// an inline array before this, so reaching one here is a mismatch between
    /// the two and is reported rather than guessed at.
    pub(super) fn foreign_member_type(
        &self,
        aggregate: kira_runtime_abi::ForeignAggregateId,
        member: u32,
    ) -> Option<ForeignType> {
        match self
            .program
            .foreign_aggregates
            .get(aggregate)?
            .members()
            .get(member as usize)?
        {
            ForeignMember::Scalar(ty) => Some(*ty),
            ForeignMember::Aggregate(_) | ForeignMember::Array { .. } => None,
        }
    }
}
