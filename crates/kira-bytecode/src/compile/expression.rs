//! Expression and call lowering.

use kira_ir::{IrBinOp, IrCallee, IrExpr, IrExprId, IrWriteback};
use kira_runtime_abi::{Execution, ForeignMember, ForeignPointerWidth, ForeignType};
use kira_semantics_model::Type;
use kira_semantics_model::hir::FieldOrder;

use crate::op::{Instruction, WritebackTarget};

use super::{CompileError, FnCompiler, unary_instruction};

impl FnCompiler<'_> {
    pub(super) fn compile_expr(&mut self, id: IrExprId) -> Result<(), CompileError> {
        match self.program.expr(id) {
            IrExpr::Int(value) => self.code.push(Instruction::ConstInt(*value)),
            IrExpr::Float(value) => self.code.push(Instruction::ConstFloat(*value)),
            IrExpr::Bool(value) => self.code.push(Instruction::ConstBool(*value)),
            IrExpr::Str(value) => {
                let pool = self.strings.intern(value);
                self.code.push(Instruction::ConstStr(pool));
            }
            IrExpr::RawPtrNull => self.code.push(Instruction::ConstRawPtrNull),
            IrExpr::ForeignCallbackPtr { callback } => {
                self.code.push(Instruction::ForeignCallback(*callback));
            }
            IrExpr::Local(slot) => {
                let takes = self.local_is_taken(*slot);
                let slot = self.local_slot(*slot)?;
                self.code.push(match takes {
                    true => Instruction::TakeLocal(slot),
                    false => Instruction::LoadLocal(slot),
                });
            }
            IrExpr::ConstantGet { constant, .. } => {
                self.code
                    .push(Instruction::LoadConstant(u64::from(*constant)));
            }
            IrExpr::CellNew { value, .. } => {
                let value = *value;
                self.compile_expr(value)?;
                self.code.push(Instruction::NewCell);
            }
            IrExpr::CellNull { .. } => self.code.push(Instruction::ConstRawPtrNull),
            IrExpr::CellGet { slot, .. } => {
                let slot = self.local_slot(*slot)?;
                self.code.push(Instruction::CellGet(slot));
            }
            IrExpr::Unary { op, operand, ty } => {
                let (op, operand, ty) = (*op, *operand, *ty);
                self.compile_expr(operand)?;
                self.code.push(unary_instruction(op));
                // The operator's *result* type, not its operand's: `~` answers
                // `Int` whatever width it was handed, and checking the operand's
                // width here is what made the VM trap where native did not.
                self.check_width(kira_ir::unary_result_type(op, ty));
            }
            IrExpr::Binary { op, lhs, rhs, ty } => self.compile_binary(*op, *lhs, *rhs, *ty)?,
            IrExpr::Select {
                cond,
                then,
                otherwise,
                ..
            } => self.compile_select(*cond, *then, *otherwise)?,
            IrExpr::StructNew {
                struct_id,
                fields,
                order,
            } => {
                let struct_id = *struct_id;
                let fields = fields.clone();
                let count = fields.len() as u64;
                // The type is known here and nowhere later: the heap object the
                // VM builds carries no type, so a user `Drop` body has to be
                // recorded on it at construction or it can never be found.
                let glue = self
                    .program
                    .types
                    .structs()
                    .get(struct_id)
                    .and_then(|def| def.drop_glue);
                if let FieldOrder::Written(order) = order {
                    // The initializers run as written; the instruction puts
                    // each popped value into its declared field.
                    let order = order.clone();
                    for &slot in &order {
                        self.compile_expr(fields[slot as usize])?;
                    }
                    self.code.push(Instruction::NewStructOrdered {
                        order: order.into_iter().map(u64::from).collect(),
                        glue,
                    });
                    return Ok(());
                }
                // Fields are pushed in declaration order, so the struct the VM
                // builds has them in layout order with no reordering.
                for field in fields {
                    self.compile_expr(field)?;
                }
                match glue {
                    Some(glue) => self.code.push(Instruction::NewStructDropping {
                        fields: count,
                        glue,
                    }),
                    None => self.code.push(Instruction::NewStruct(count)),
                }
            }
            IrExpr::Field { base, index, .. } => {
                let base = *base;
                let index = self.field_index(*index)?;
                // Reading a member does not consume the value it is read from,
                // so the base is borrowed rather than taken.
                self.compile_borrowed_expr(base)?;
                self.code.push(Instruction::GetField(index));
            }
            IrExpr::ArrayElements { value, element } => {
                let (value, element) = (*value, *element);
                self.compile_expr(value)?;
                self.code.push(Instruction::ArrayElements(element));
            }
            IrExpr::ScalarText { value } => {
                let value = *value;
                self.compile_expr(value)?;
                self.code.push(Instruction::ScalarText);
            }
            IrExpr::MathOperation { op, operands } => {
                let (op, operands) = (*op, operands.clone());
                // Pushed in source order, so `pow(x, y)` leaves the exponent on
                // top and the instruction pops back down to the base.
                for operand in operands {
                    self.compile_expr(operand)?;
                }
                self.code.push(Instruction::MathOp(op));
            }
            IrExpr::ForeignMemberAddress {
                base,
                aggregate,
                member,
                ..
            } => {
                let (base, aggregate, member) = (*base, *aggregate, *member);
                let offset = self.foreign_member_offset(aggregate, member)?;
                self.compile_expr(base)?;
                self.code.push(Instruction::ForeignOffset(offset));
            }
            IrExpr::ForeignElement {
                base,
                aggregate,
                index,
                ..
            } => {
                let (base, aggregate, index) = (*base, *aggregate, *index);
                // The VM runs on the host, so the host's pointer width is the
                // one this bytecode is executed with.
                let stride = self
                    .program
                    .foreign_aggregates
                    .layout_of(aggregate, ForeignPointerWidth::HOST)
                    .map_err(|_| CompileError::ForeignMemberMissing {
                        function: self.function_name.to_owned(),
                        member: 0,
                    })?
                    .size;
                self.compile_expr(base)?;
                self.compile_expr(index)?;
                self.code.push(Instruction::ForeignIndex(stride));
            }
            IrExpr::ForeignField {
                base,
                aggregate,
                member,
                ..
            } => {
                let (base, aggregate, member) = (*base, *aggregate, *member);
                // The VM runs on the host, so the host's pointer width is the
                // one this bytecode will be executed with.
                let offset = self
                    .program
                    .foreign_aggregates
                    .member_offsets_of(aggregate, ForeignPointerWidth::HOST)
                    .ok()
                    .and_then(|offsets| offsets.get(member as usize).copied());
                let (Some(offset), Some(ty)) =
                    (offset, self.foreign_member_type(aggregate, member))
                else {
                    return Err(CompileError::ForeignMemberMissing {
                        function: self.function_name.to_owned(),
                        member,
                    });
                };
                self.compile_expr(base)?;
                self.code.push(Instruction::ForeignLoad { offset, ty });
            }
            IrExpr::ArrayNew { elements, .. } => {
                let elements = elements.clone();
                let count = elements.len() as u64;
                // Elements are pushed in written order, so the array the VM
                // builds is in that order with no reordering.
                for element in elements {
                    self.compile_expr(element)?;
                }
                self.code.push(Instruction::NewArray(count));
            }
            IrExpr::Index { base, index, .. } => {
                let (base, index) = (*base, *index);
                // A base that is just a local is borrowed rather than copied:
                // `LoadLocal` copies the whole array, so reading one element
                // through it costs the whole array and a loop over `n` elements
                // costs `O(n²)`. Only the element is copied out either way,
                // which is what keeps the handed-out value unshared.
                if let IrExpr::Local(slot) = *self.program.expr(base) {
                    let slot = self.local_slot(slot)?;
                    self.compile_expr(index)?;
                    self.code.push(Instruction::ArrayGetLocal(slot));
                } else {
                    self.compile_expr(base)?;
                    self.compile_expr(index)?;
                    self.code.push(Instruction::ArrayGet);
                }
            }
            IrExpr::TaskOp { prim, operands } => {
                let prim = *prim;
                let operands = *operands;
                // Three operands, deepest first, exactly as a three-argument
                // call would push them — so the instruction pops them in the
                // one order both engines already agree on.
                for operand in operands {
                    self.compile_expr(operand)?;
                }
                self.code.push(Instruction::TaskOp(prim));
            }
            IrExpr::ChannelOp { prim, operands } => {
                let prim = *prim;
                let operands = *operands;
                // The same deepest-first push order `TaskOp` uses.
                for operand in operands {
                    self.compile_expr(operand)?;
                }
                self.code.push(Instruction::ChannelOp(prim));
            }
            IrExpr::MainThreadCall {
                operation,
                function,
                args,
                ..
            } => self.compile_main_thread_call(*operation, *function, args)?,
            IrExpr::MainThreadJoin { handle, .. } => self.compile_main_thread_join(*handle)?,
            IrExpr::ArrayLen { array } => {
                let array = *array;
                // Counting an array does not consume it, so the base is
                // borrowed: taking it would move an array of `Drop` elements
                // out of its local and release it at the end of the count.
                self.compile_borrowed_expr(array)?;
                self.code.push(Instruction::ArrayLen);
            }
            IrExpr::StringLen { text } => {
                let text = *text;
                self.compile_expr(text)?;
                self.code.push(Instruction::StringLen);
            }
            IrExpr::StringCharAt { text, index } => {
                let (text, index) = (*text, *index);
                self.compile_expr(text)?;
                self.compile_expr(index)?;
                self.code.push(Instruction::StringCharAt);
            }
            IrExpr::StringSubstring { text, start, end } => {
                let (text, start, end) = (*text, *start, *end);
                self.compile_expr(text)?;
                self.compile_expr(start)?;
                self.compile_expr(end)?;
                self.code.push(Instruction::StringSubstring);
            }
            IrExpr::StringIndexOf { text, needle } => {
                let (text, needle) = (*text, *needle);
                self.compile_expr(text)?;
                self.compile_expr(needle)?;
                self.code.push(Instruction::StringIndexOf);
            }
            IrExpr::StringOperation {
                op,
                text,
                arguments,
                ..
            } => {
                let (op, text) = (*op, *text);
                let arguments = arguments.clone();
                self.compile_expr(text)?;
                for argument in arguments {
                    self.compile_expr(argument)?;
                }
                self.code.push(Instruction::StringOp(op));
            }
            IrExpr::StringOf { value } => {
                let value = *value;
                let unsigned = self.program.expr_type(self.function, value)
                    == Type::Int(kira_semantics_model::IntSpelling::U64);
                self.compile_expr(value)?;
                self.code.push(if unsigned {
                    Instruction::StringOfUnsigned
                } else {
                    Instruction::StringOf
                });
            }
            IrExpr::CStringNew { text } => {
                let text = *text;
                self.compile_expr(text)?;
                self.code.push(Instruction::CStringNew);
            }
            IrExpr::CLayoutAddress { value, aggregate } => {
                let (value, aggregate) = (*value, *aggregate);
                self.compile_expr(value)?;
                self.code.push(Instruction::CLayoutAddress(aggregate.0));
            }
            IrExpr::FileSystem { op, args, .. } => {
                let (op, args) = (*op, args.clone());
                for arg in args {
                    self.compile_expr(arg)?;
                }
                self.code.push(Instruction::FileSystem(op));
            }
            IrExpr::Compiler { op, args, .. } => {
                let (op, args) = (*op, args.clone());
                for arg in args {
                    self.compile_expr(arg)?;
                }
                self.code.push(Instruction::Compiler(op));
            }
            IrExpr::Env { op, args, .. } => {
                let (op, args) = (*op, args.clone());
                for arg in args {
                    self.compile_expr(arg)?;
                }
                self.code.push(Instruction::Env(op));
            }
            IrExpr::NativeState { value, type_id, .. } => {
                let (value, type_id) = (*value, *type_id);
                self.compile_expr(value)?;
                self.code.push(Instruction::NativeState(type_id.as_word()));
            }
            IrExpr::NativeUserData { state } => {
                let state = *state;
                // `LoadLocal` copies a handle and records its retain. The token
                // takes that copied reference; a temporary hands over the
                // reference it already owns. Neither path needs another retain
                // in `NativeUserData`.
                match self.program.expr(state) {
                    IrExpr::Local(slot) if self.local_is_taken(*slot) => {
                        let slot = self.local_slot(*slot)?;
                        self.code.push(Instruction::LoadLocal(slot));
                    }
                    _ => {
                        self.compile_expr(state)?;
                    }
                }
                self.code
                    .push(Instruction::NativeUserData { shared: false });
            }
            IrExpr::NativeRecover { raw, type_id, .. } => {
                let (raw, type_id) = (*raw, *type_id);
                self.compile_expr(raw)?;
                self.code
                    .push(Instruction::NativeRecover(type_id.as_word()));
            }
            IrExpr::NativeStateTake { raw, type_id, .. } => {
                let (raw, type_id) = (*raw, *type_id);
                self.compile_expr(raw)?;
                self.code
                    .push(Instruction::NativeStateTake(type_id.as_word()));
            }
            IrExpr::NativeStateRetain { token } => {
                let token = *token;
                self.compile_expr(token)?;
                self.code.push(Instruction::NativeStateRetain);
            }
            IrExpr::NativeStateRelease { token } => {
                let token = *token;
                self.compile_expr(token)?;
                self.code.push(Instruction::NativeStateRelease);
            }
            IrExpr::Convert { operand, kind, ty } => {
                let (operand, kind, ty) = (*operand, *kind, *ty);
                let from = self.program.expr_type(self.function, operand);
                self.compile_expr(operand)?;
                self.compile_convert(kind, from, ty);
            }
            // Erasure boxes on this side too, carrying the type that crossed
            // in. It did not always: a `Value` is a tagged union, so the erased
            // form of a value *was* that value and this emitted nothing.
            //
            // What that could not answer is which *declaration* wrote a struct.
            // A struct object here is a tuple of values — the VM is
            // structurally typed on purpose — so `Point(1, 2)` and `Rect(1, 2)`
            // are indistinguishable once erased, and `EqAny` would have to call
            // them equal where the LLVM backend, holding an aggregate as
            // untyped bytes plus generated leaves, cannot read one as the other
            // at all. The id written here is what lets both engines answer
            // alike. Carrying a value still costs only the box; comparing one
            // is what needed the type.
            //
            // See `kira_semantics_model::ErasedTypeId` for the encoding, and
            IrExpr::TypeTest { value, target } => {
                let (value, target) = (*value, *target);
                self.compile_expr(value)?;
                self.code.push(Instruction::TypeTest(target.as_u64()));
            }
            IrExpr::TypeCast { value, target, .. } => {
                let (value, target) = (*value, *target);
                self.compile_expr(value)?;
                self.code.push(Instruction::Downcast(target.as_u64()));
            }
            // `Heap::alloc_erased` for what the box holds.
            IrExpr::IntoAny { value, tag, .. } => {
                let (value, tag) = (*value, *tag);
                self.compile_expr(value)?;
                self.code.push(Instruction::Erase(tag.as_u64()));
            }
            // The value is evaluated for its effects and dropped; the answer
            // was settled when lowering interned its type.
            IrExpr::TypeConst { value, id } => {
                let (value, id) = (*value, *id);
                self.compile_expr(value)?;
                self.code.push(Instruction::Pop);
                self.code.push(Instruction::ConstType(id.as_u64()));
            }
            IrExpr::TypeOf { value } => {
                let value = *value;
                self.compile_expr(value)?;
                self.code.push(Instruction::TypeOf);
            }
            // The cast that answers instead of trapping. `TypeCastResult`
            // leaves the payload or the descriptor under a `Bool`, and the
            // branch below wraps whichever it left: `Ok(payload)` on one side,
            // `Error(Mismatch(type))` on the other. Both sides leave one value,
            // so the join is implicit exactly as a conditional's is.
            IrExpr::TypeCastResult { value, target, .. } => {
                let (value, target) = (*value, *target);
                self.compile_expr(value)?;
                self.code.push(Instruction::TypeCastResult(target.as_u64()));
                let to_error = self.emit_placeholder_jump_if_false();
                self.code.push(Instruction::NewEnum {
                    tag: u64::from(kira_semantics_model::cast_result::OK_TAG),
                    has_payload: true,
                });
                let to_end = self.emit_placeholder_jump();
                self.patch_to_here(to_error)?;
                self.code.push(Instruction::NewEnum {
                    tag: u64::from(kira_semantics_model::cast_result::MISMATCH_TAG),
                    has_payload: true,
                });
                self.code.push(Instruction::NewEnum {
                    tag: u64::from(kira_semantics_model::cast_result::ERROR_TAG),
                    has_payload: true,
                });
                self.patch_to_here(to_end)?;
            }
            IrExpr::TypeField {
                descriptor, field, ..
            } => {
                let (descriptor, field) = (*descriptor, *field);
                self.compile_expr(descriptor)?;
                self.code.push(Instruction::TypeField(field.as_byte()));
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
                // The payload, when present, is pushed first so it is on top of
                // the stack for `NewEnum` to take, exactly as a struct's fields
                // are pushed before `NewStruct`.
                if let Some(payload) = payload {
                    self.compile_expr(payload)?;
                }
                self.code.push(Instruction::NewEnum {
                    tag: u64::from(tag),
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
                writebacks,
                ..
            } => {
                let callee = *callee;
                let args = args.clone();
                // A call the callee writes through carries its writebacks; it
                // compiles to `CallMut` or `CallWriteback`, which thread the
                // written-through parameters back after the call.
                if !writebacks.is_empty() {
                    let writebacks = writebacks.clone();
                    return self.compile_writeback_call(callee, &args, &writebacks);
                }
                // A `U64` prints as the unsigned value it is; the word alone
                // cannot say so, so the instruction does.
                let prints_unsigned = callee == IrCallee::Print
                    && args.first().is_some_and(|&arg| {
                        self.program.expr_type(self.function, arg)
                            == Type::Int(kira_semantics_model::IntSpelling::U64)
                    });
                for (position, arg) in args.into_iter().enumerate() {
                    match self.argument_is_borrowed(callee, position) {
                        true => self.compile_borrowed_expr(arg)?,
                        false => self.compile_expr(arg)?,
                    }
                }
                match callee {
                    IrCallee::Print if prints_unsigned => {
                        self.code.push(Instruction::PrintUnsigned);
                    }
                    IrCallee::Print => self.code.push(Instruction::Print),
                    // Which engine owns the callee is known here, at compile
                    // time, so the boundary costs a different opcode rather
                    // than a branch on every call.
                    IrCallee::User(index) => {
                        // Every call to a function with a by-reference parameter
                        // carries writebacks, handled above; one reaching here
                        // without them would compile to a plain `Call` and
                        // silently lose the mutation, so it is refused instead.
                        if self.function_writes_back(index) {
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
                            Instruction::Call(u64::from(index))
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
    fn compile_writeback_call(
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
    fn compile_writeback_argument(&mut self, arg: IrExprId) -> Result<(), CompileError> {
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
    fn function_writes_back(&self, index: u32) -> bool {
        self.program
            .functions
            .get(index as usize)
            .is_some_and(|function| !function.by_reference_params.is_empty())
    }

    fn compile_binary(
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

    /// The byte offset of one member of a C-layout aggregate.
    fn foreign_member_offset(
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
    fn foreign_member_type(
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
