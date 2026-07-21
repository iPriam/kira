//! Expression and call lowering.

use kira_ir::{ConvertKind, IrBinOp, IrCallee, IrExpr, IrExprId, IrPlace};
use kira_runtime_abi::Execution;

use crate::op::Instruction;

use super::{CompileError, FnCompiler, binary_instruction, unary_instruction};

impl FnCompiler<'_> {
    pub(super) fn compile_expr(&mut self, id: IrExprId) -> Result<(), CompileError> {
        match self.program.expr(id) {
            IrExpr::Int(value) => self.code.push(Instruction::ConstInt(*value)),
            IrExpr::Float(value) => self.code.push(Instruction::ConstFloat(*value)),
            IrExpr::Bool(value) => self.code.push(Instruction::ConstBool(*value)),
            IrExpr::Str(value) => {
                let pool = self.strings.intern(value)?;
                self.code.push(Instruction::ConstStr(pool));
            }
            IrExpr::Local(slot) => {
                let slot = self.local_slot(*slot)?;
                self.code.push(Instruction::LoadLocal(slot));
            }
            IrExpr::Unary { op, operand } => {
                let operand = *operand;
                let op = *op;
                self.compile_expr(operand)?;
                self.code.push(unary_instruction(op));
            }
            IrExpr::Binary { op, lhs, rhs } => self.compile_binary(*op, *lhs, *rhs)?,
            IrExpr::Select {
                cond,
                then,
                otherwise,
                ..
            } => self.compile_select(*cond, *then, *otherwise)?,
            IrExpr::StructNew { fields, .. } => {
                let fields = fields.clone();
                let count =
                    u16::try_from(fields.len()).map_err(|_| CompileError::TooManyFields {
                        function: self.function_name.to_owned(),
                        count: fields.len(),
                    })?;
                // Fields are pushed in declaration order, so the struct the VM
                // builds has them in layout order with no reordering.
                for field in fields {
                    self.compile_expr(field)?;
                }
                self.code.push(Instruction::NewStruct(count));
            }
            IrExpr::Field { base, index, .. } => {
                let base = *base;
                let index = self.field_index(*index)?;
                self.compile_expr(base)?;
                self.code.push(Instruction::GetField(index));
            }
            IrExpr::ArrayNew { elements, .. } => {
                let elements = elements.clone();
                let count =
                    u32::try_from(elements.len()).map_err(|_| CompileError::TooManyElements {
                        function: self.function_name.to_owned(),
                        count: elements.len(),
                    })?;
                // Elements are pushed in written order, so the array the VM
                // builds is in that order with no reordering.
                for element in elements {
                    self.compile_expr(element)?;
                }
                self.code.push(Instruction::NewArray(count));
            }
            IrExpr::Index { base, index, .. } => {
                let (base, index) = (*base, *index);
                self.compile_expr(base)?;
                self.compile_expr(index)?;
                self.code.push(Instruction::ArrayGet);
            }
            IrExpr::ArrayLen { array } => {
                let array = *array;
                self.compile_expr(array)?;
                self.code.push(Instruction::ArrayLen);
            }
            IrExpr::NativeState { value, type_id, .. } => {
                let (value, type_id) = (*value, *type_id);
                self.compile_expr(value)?;
                self.code.push(Instruction::NativeState(type_id.as_word()));
            }
            IrExpr::NativeUserData { state } => {
                let state = *state;
                self.compile_expr(state)?;
                self.code.push(Instruction::NativeUserData);
            }
            IrExpr::NativeRecover { raw, type_id, .. } => {
                let (raw, type_id) = (*raw, *type_id);
                self.compile_expr(raw)?;
                self.code
                    .push(Instruction::NativeRecover(type_id.as_word()));
            }
            IrExpr::NativeStateFree { token } => {
                let token = *token;
                self.compile_expr(token)?;
                self.code.push(Instruction::NativeStateFree);
            }
            IrExpr::Convert { operand, kind, .. } => {
                let (operand, kind) = (*operand, *kind);
                self.compile_expr(operand)?;
                // An integer-width or float-width conversion is an identity copy
                // over one runtime representation, so it emits nothing; only the
                // two cross-representation conversions have an instruction.
                match kind {
                    ConvertKind::IntToInt | ConvertKind::FloatToFloat => {}
                    ConvertKind::IntToFloat => self.code.push(Instruction::ConvertIntToFloat),
                    ConvertKind::FloatToInt => self.code.push(Instruction::ConvertFloatToInt),
                }
            }
            IrExpr::ArrayAppend { place, value } => {
                let (place, value) = (place.clone(), *value);
                let slot = self.local_slot(place.local)?;
                let path = self.compile_place_indices(&place)?;
                self.compile_expr(value)?;
                self.code.push(Instruction::ArrayAppend { slot, path });
                // `append` yields `Void`, and every expression leaves exactly
                // one value: the statement that discards it pops this.
                self.code.push(Instruction::ConstVoid);
            }
            IrExpr::EnumNew { tag, payload, .. } => {
                let (tag, payload) = (*tag, *payload);
                let tag = u16::try_from(tag).map_err(|_| CompileError::TooManyVariants {
                    function: self.function_name.to_owned(),
                    tag,
                })?;
                // The payload, when present, is pushed first so it is on top of
                // the stack for `NewEnum` to take, exactly as a struct's fields
                // are pushed before `NewStruct`.
                if let Some(payload) = payload {
                    self.compile_expr(payload)?;
                }
                self.code.push(Instruction::NewEnum {
                    tag,
                    has_payload: payload.is_some(),
                });
            }
            IrExpr::EnumTag { value } => {
                let value = *value;
                self.compile_expr(value)?;
                self.code.push(Instruction::EnumTag);
            }
            IrExpr::EnumPayload { value, .. } => {
                // The payload's type is a backend concern only where values are
                // typed statically; a VM `Value` describes itself, so the
                // instruction needs no operand.
                let value = *value;
                self.compile_expr(value)?;
                self.code.push(Instruction::EnumPayload);
            }
            IrExpr::Call {
                callee,
                args,
                writeback,
                ..
            } => {
                let callee = *callee;
                let args = args.clone();
                // A call that mutates its receiver carries the writeback place;
                // it compiles to `CallMut`, which threads the mutated receiver
                // back after the call.
                if let Some(place) = writeback.clone() {
                    return self.compile_mut_call(callee, &args, &place);
                }
                for arg in args {
                    self.compile_expr(arg)?;
                }
                match callee {
                    IrCallee::Print => self.code.push(Instruction::Print),
                    // Which engine owns the callee is known here, at compile
                    // time, so the boundary costs a different opcode rather
                    // than a branch on every call.
                    IrCallee::User(index) => {
                        // Every call to a mutating method carries a writeback,
                        // handled above; one reaching here without it would
                        // compile to a plain `Call` and silently lose the
                        // mutation, so it is refused instead.
                        if self.function_mutates_self(index) {
                            return Err(CompileError::MalformedMutCall {
                                function: self.function_name.to_owned(),
                            });
                        }
                        let native = self
                            .engines
                            .get(index as usize)
                            .is_some_and(|engine| *engine == Execution::Native);
                        self.code.push(if native {
                            Instruction::CallNative(index)
                        } else {
                            Instruction::Call(index)
                        });
                    }
                    // A foreign call names a foreign-import id; arguments are
                    // already on the stack, and the VM marshals them to the
                    // import's signature before asking the host.
                    IrCallee::Foreign(id) => self.code.push(Instruction::CallForeign(id)),
                }
            }
        }
        Ok(())
    }

    /// Compiles a call to a mutating method into a [`Instruction::CallMut`].
    ///
    /// The arguments — the receiver copy first, then the rest — are pushed
    /// exactly as an ordinary call pushes them; the writeback place's index
    /// expressions follow, per the place convention, so the runtime pops the
    /// indices off the top before the arguments. The callee's mutated receiver
    /// (its slot 0) is written back through the place when it returns.
    fn compile_mut_call(
        &mut self,
        callee: IrCallee,
        args: &[IrExprId],
        place: &IrPlace,
    ) -> Result<(), CompileError> {
        // Only a user method is ever a mutating callee: `print` and a foreign
        // function have no receiver to write back.
        let IrCallee::User(index) = callee else {
            return Err(CompileError::MalformedMutCall {
                function: self.function_name.to_owned(),
            });
        };
        // A struct receiver cannot cross the seam, so a mutating call is always
        // same-engine; a native callee here means the split misplaced one.
        if self
            .engines
            .get(index as usize)
            .is_some_and(|engine| *engine == Execution::Native)
        {
            return Err(CompileError::MutCallAcrossSeam {
                function: self.function_name.to_owned(),
            });
        }
        for &arg in args {
            self.compile_expr(arg)?;
        }
        let slot = self.local_slot(place.local)?;
        let path = self.compile_place_indices(place)?;
        self.code.push(Instruction::CallMut {
            func: index,
            slot,
            path,
        });
        Ok(())
    }

    /// Whether the function at `index` is a mutating method.
    fn function_mutates_self(&self, index: u32) -> bool {
        self.program
            .functions
            .get(index as usize)
            .is_some_and(|function| function.mutates_self)
    }

    fn compile_binary(
        &mut self,
        op: IrBinOp,
        lhs: IrExprId,
        rhs: IrExprId,
    ) -> Result<(), CompileError> {
        match op {
            IrBinOp::And => self.compile_and(lhs, rhs),
            IrBinOp::Or => self.compile_or(lhs, rhs),
            other => {
                self.compile_expr(lhs)?;
                self.compile_expr(rhs)?;
                self.code.push(binary_instruction(other)?);
                Ok(())
            }
        }
    }

    /// `a && b`: evaluate `b` only when `a` is true.
    fn compile_and(&mut self, lhs: IrExprId, rhs: IrExprId) -> Result<(), CompileError> {
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
    fn compile_select(
        &mut self,
        cond: IrExprId,
        then: IrExprId,
        otherwise: IrExprId,
    ) -> Result<(), CompileError> {
        self.compile_expr(cond)?;
        let to_else = self.emit_placeholder_jump_if_false();
        self.compile_expr(then)?;
        let to_end = self.emit_placeholder_jump();
        self.patch_to_here(to_else)?;
        self.compile_expr(otherwise)?;
        self.patch_to_here(to_end)
    }

    /// `a || b`: evaluate `b` only when `a` is false.
    fn compile_or(&mut self, lhs: IrExprId, rhs: IrExprId) -> Result<(), CompileError> {
        self.compile_expr(lhs)?;
        let to_rhs = self.emit_placeholder_jump_if_false();
        self.code.push(Instruction::ConstBool(true));
        let to_end = self.emit_placeholder_jump();
        self.patch_to_here(to_rhs)?;
        self.compile_expr(rhs)?;
        self.patch_to_here(to_end)
    }
}
