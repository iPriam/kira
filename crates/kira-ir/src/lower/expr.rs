use super::*;

impl Lowerer<'_> {
    pub(super) fn lower_expr(&mut self, id: HirExprId) -> IrExprId {
        let node = match self.hir.expr(id).clone() {
            // A `distinct` crossing lowers to the value that crossed, and to
            // nothing else. `TabId(word)` and `id.raw` are the same bits either
            // way, so the node that carried the type through the type checker
            // has no instruction to become: it disappears here, which is the
            // whole of what makes a distinct type cost nothing.
            HirExpr::Distinct { value, .. } => return self.lower_expr(value),
            HirExpr::Int(value) => IrExpr::Int(value),
            HirExpr::Float(value) => IrExpr::Float(value),
            HirExpr::Bool(value) => IrExpr::Bool(value),
            HirExpr::Str(value) => IrExpr::Str(value),
            HirExpr::RawPtrNull => IrExpr::RawPtrNull,
            HirExpr::ForeignCallbackPtr { callback } => IrExpr::ForeignCallbackPtr { callback },
            HirExpr::Local { local, .. } => IrExpr::Local(self.slot(local.0)),
            HirExpr::ConstantGet { constant, ty } => IrExpr::ConstantGet { constant, ty },
            HirExpr::CellNew { value, ty } => IrExpr::CellNew {
                value: self.lower_expr(value),
                ty,
            },
            HirExpr::CellNull { ty } => IrExpr::CellNull { ty },
            HirExpr::CellGet { local, ty } => IrExpr::CellGet {
                slot: self.slot(local.0),
                ty,
            },
            // A copy of a Copyable value is the value: every engine's read of a
            // scalar is a copy, and a string or array read shares until written.
            HirExpr::Copy { value, .. } => return self.lower_expr(value),
            HirExpr::TypeTest { value, target } => IrExpr::TypeTest {
                value: self.lower_expr(value),
                target: self.descriptor_of(target),
            },
            HirExpr::TypeCast { value, target } => IrExpr::TypeCast {
                value: self.lower_expr(value),
                target: self.descriptor_of(target),
                ty: target,
            },
            // `value.type` on a value whose type is known needs no runtime
            // question: the answer is the id lowering just interned, and the
            // operand is still evaluated and released for its effects.
            HirExpr::TypeCastResult {
                value,
                target,
                failure,
                ty,
            } => {
                let Type::Enum(result) = ty else {
                    // Analysis mints the row before it builds the node, so a
                    // non-enum here is a lowering that skipped it.
                    return self.ir.exprs.alloc(IrExpr::Int(0));
                };
                IrExpr::TypeCastResult {
                    value: self.lower_expr(value),
                    target: self.descriptor_of(target),
                    result,
                    failure,
                    payload: target,
                }
            }
            HirExpr::TypeField {
                descriptor,
                field,
                ty,
            } => IrExpr::TypeField {
                descriptor: self.lower_expr(descriptor),
                field,
                ty,
            },
            HirExpr::TypeOf { value, of } => match of {
                Type::Any => IrExpr::TypeOf {
                    value: self.lower_expr(value),
                },
                known => IrExpr::TypeConst {
                    value: self.lower_expr(value),
                    id: self.descriptor_of(known),
                },
            },
            HirExpr::Unary { op, operand, ty } => IrExpr::Unary {
                op,
                operand: self.lower_expr(operand),
                ty,
            },
            HirExpr::Binary { op, lhs, rhs, ty } => IrExpr::Binary {
                op,
                lhs: self.lower_expr(lhs),
                rhs: self.lower_expr(rhs),
                ty,
            },
            HirExpr::Select {
                cond,
                then_setup,
                then,
                then_cleanup,
                otherwise_setup,
                otherwise,
                otherwise_cleanup,
                ty,
            } => IrExpr::Select {
                cond: self.lower_expr(cond),
                then_setup: self.lower_stmts(&then_setup),
                then: self.lower_expr(then),
                then_cleanup: then_cleanup.into_iter().map(|local| local.0).collect(),
                otherwise_setup: self.lower_stmts(&otherwise_setup),
                otherwise: self.lower_expr(otherwise),
                otherwise_cleanup: otherwise_cleanup.into_iter().map(|local| local.0).collect(),
                ty,
            },
            HirExpr::Call {
                callee,
                args,
                ty,
                writebacks,
            } => {
                let ir_args = args.iter().map(|&arg| self.lower_expr(arg)).collect();
                let writebacks = writebacks
                    .iter()
                    .map(|writeback| IrWriteback {
                        param: writeback.param,
                        place: self.lower_place(&writeback.place),
                    })
                    .collect();
                IrExpr::Call {
                    callee: self.lower_callee(callee),
                    args: ir_args,
                    result: ty,
                    writebacks,
                }
            }
            HirExpr::StructNew {
                struct_id,
                fields,
                order,
            } => {
                let ir_fields = fields.iter().map(|&field| self.lower_expr(field)).collect();
                IrExpr::StructNew {
                    struct_id,
                    fields: ir_fields,
                    order,
                }
            }
            HirExpr::Field { base, index, ty } => IrExpr::Field {
                base: self.lower_expr(base),
                index,
                ty,
            },
            HirExpr::ForeignMemberAddress {
                base,
                aggregate,
                member,
                ty,
            } => IrExpr::ForeignMemberAddress {
                base: self.lower_expr(base),
                aggregate,
                member,
                ty,
            },
            HirExpr::ForeignElement {
                base,
                aggregate,
                index,
                ty,
            } => IrExpr::ForeignElement {
                base: self.lower_expr(base),
                aggregate,
                index: self.lower_expr(index),
                ty,
            },
            HirExpr::ArrayElements { value, element } => IrExpr::ArrayElements {
                value: self.lower_expr(value),
                element,
            },
            HirExpr::ScalarText { value } => IrExpr::ScalarText {
                value: self.lower_expr(value),
            },
            HirExpr::MathOperation { op, operands } => IrExpr::MathOperation {
                op,
                operands: operands
                    .into_iter()
                    .map(|operand| self.lower_expr(operand))
                    .collect(),
            },
            HirExpr::ForeignField {
                base,
                aggregate,
                member,
                ty,
            } => IrExpr::ForeignField {
                base: self.lower_expr(base),
                aggregate,
                member,
                ty,
            },
            HirExpr::ArrayNew { ty, elements } => {
                let ir_elements = elements
                    .iter()
                    .map(|&element| self.lower_expr(element))
                    .collect();
                IrExpr::ArrayNew {
                    ty,
                    elements: ir_elements,
                }
            }
            HirExpr::Index { base, index, ty } => IrExpr::Index {
                base: self.lower_expr(base),
                index: self.lower_expr(index),
                ty,
            },
            HirExpr::EnumNew {
                enum_id,
                tag,
                payload,
            } => IrExpr::EnumNew {
                enum_id,
                tag,
                payload: payload.map(|expr| self.lower_expr(expr)),
            },
            HirExpr::EnumTag { value } => IrExpr::EnumTag {
                value: self.lower_expr(value),
            },
            HirExpr::EnumPayload { value, ty } => IrExpr::EnumPayload {
                value: self.lower_expr(value),
                ty,
            },
            HirExpr::ArrayLen { array } => IrExpr::ArrayLen {
                array: self.lower_expr(array),
            },
            HirExpr::StringCharAt { text, index } => IrExpr::StringCharAt {
                text: self.lower_expr(text),
                index: self.lower_expr(index),
            },
            HirExpr::StringSubstring { text, start, end } => IrExpr::StringSubstring {
                text: self.lower_expr(text),
                start: self.lower_expr(start),
                end: self.lower_expr(end),
            },
            HirExpr::StringIndexOf { text, needle } => IrExpr::StringIndexOf {
                text: self.lower_expr(text),
                needle: self.lower_expr(needle),
            },
            HirExpr::StringOperation {
                op,
                text,
                arguments,
                ty,
            } => IrExpr::StringOperation {
                op,
                text: self.lower_expr(text),
                arguments: arguments
                    .into_iter()
                    .map(|argument| self.lower_expr(argument))
                    .collect(),
                ty,
            },
            HirExpr::NumberOperation { op, operands, ty } => IrExpr::NumberOperation {
                op,
                operands: operands
                    .into_iter()
                    .map(|operand| self.lower_expr(operand))
                    .collect(),
                ty,
            },
            HirExpr::StringOf { value } => IrExpr::StringOf {
                value: self.lower_expr(value),
            },
            HirExpr::StringLen { text } => IrExpr::StringLen {
                text: self.lower_expr(text),
            },
            HirExpr::CLayoutAddress { value, aggregate } => IrExpr::CLayoutAddress {
                value: self.lower_expr(value),
                aggregate,
            },
            HirExpr::CStringNew { text } => IrExpr::CStringNew {
                text: self.lower_expr(text),
            },
            // A null C string and a null pointer are one zero word, so this
            // needs no node of its own below the type checker.
            HirExpr::CStringNull => IrExpr::RawPtrNull,
            HirExpr::FileSystem { op, args, ty } => IrExpr::FileSystem {
                op,
                args: args.into_iter().map(|arg| self.lower_expr(arg)).collect(),
                ty,
            },
            HirExpr::Compiler { op, args, ty } => IrExpr::Compiler {
                op,
                args: args.into_iter().map(|arg| self.lower_expr(arg)).collect(),
                ty,
            },
            HirExpr::Env { op, args, ty } => IrExpr::Env {
                op,
                args: args.into_iter().map(|arg| self.lower_expr(arg)).collect(),
                ty,
            },
            HirExpr::ArrayAppend { place, value } => IrExpr::ArrayAppend {
                place: self.lower_place(&place),
                value: self.lower_expr(value),
            },
            HirExpr::NativeState { value, type_id, ty } => {
                let value_ty = self.hir.expr(value).type_of();
                IrExpr::NativeState {
                    value: self.lower_expr(value),
                    type_id,
                    drop_glue: self.ir.types.user_drop(value_ty),
                    ty,
                }
            }
            HirExpr::NativeUserData {
                state,
                borrowed,
                ty,
            } => IrExpr::NativeUserData {
                state: self.lower_expr(state),
                borrowed,
                ty,
            },
            HirExpr::NativeRecover { raw, type_id, ty } => IrExpr::NativeRecover {
                raw: self.lower_expr(raw),
                type_id,
                ty,
            },
            HirExpr::NativeStateRetain { token } => IrExpr::NativeStateRetain {
                token: self.lower_expr(token),
            },
            HirExpr::NativeStateRelease { token, target } => IrExpr::NativeStateRelease {
                token: self.lower_expr(token),
                target,
            },
            HirExpr::Convert { operand, kind, ty } => IrExpr::Convert {
                operand: self.lower_expr(operand),
                kind,
                ty,
            },
            HirExpr::IntoAny { value, from } => IrExpr::IntoAny {
                tag: self.descriptor_of(from),
                value: self.lower_expr(value),
                from,
            },
            HirExpr::MainThreadCall {
                operation,
                function,
                args,
                ty,
            } => IrExpr::MainThreadCall {
                operation,
                function: function.0,
                args: args.into_iter().map(|arg| self.lower_expr(arg)).collect(),
                ty,
            },
            HirExpr::MainThreadJoin { handle, ty } => IrExpr::MainThreadJoin {
                handle: self.lower_expr(handle),
                ty,
            },
            // An error node can only be reached when analysis already reported
            // diagnostics and the program is never run; lower it to a harmless
            // constant so lowering stays total.
            HirExpr::Error => IrExpr::Int(0),
            HirExpr::TaskSpawn { target, args, ty } => {
                return self.lower_task_spawn(target, &args, ty);
            }
            HirExpr::TaskJoin { handle, ty } => return self.lower_task_join(handle, ty),
            HirExpr::ChannelCreate { wire, .. } => {
                // The table is told at creation whether a queued word will be
                // a token, because a run that ends with values still queued
                // has to release what they name and there is no send left to
                // ask by then.
                let boxed = self.ir.exprs.alloc(IrExpr::Int(match wire.is_boxed() {
                    true => kira_runtime_abi::BOXED_PAYLOAD,
                    false => 0,
                }));
                return self.channel_op(ChannelPrim::Create, vec![boxed]);
            }
            HirExpr::ChannelReceiver { sender, .. } => {
                // The two ends share an index and a generation and differ only
                // in the end bit of a 1-based slot field, which makes the
                // receiver the sender's word plus one. A derivation rather than
                // a table call: there is one channel however many times this is
                // read.
                let sender = self.lower_expr(sender);
                let step = self.ir.exprs.alloc(IrExpr::Int(
                    kira_semantics_model::channel::RECEIVER_END_OFFSET,
                ));
                return self.ir.exprs.alloc(IrExpr::Binary {
                    op: IrBinOp::AddInt,
                    lhs: sender,
                    rhs: step,
                    ty: Type::INT,
                });
            }
            HirExpr::ChannelSend {
                sender,
                value,
                wire,
            } => {
                let sender = self.lower_expr(sender);
                let value_ty = self.hir.expr(value).type_of();
                let value = self.lower_expr(value);
                // One queue slot is one word, so the value becomes one: a
                // float as its bits, and a value that owns storage as a token
                // naming it in the store that outlives this context.
                let value = match wire {
                    Crossing::Word => value,
                    Crossing::FloatBits => self.ir.exprs.alloc(IrExpr::Convert {
                        operand: value,
                        kind: kira_semantics_model::hir::ConvertKind::FloatToBits,
                        ty: Type::INT,
                    }),
                    Crossing::Boxed(type_id) => {
                        let drop_glue = self.ir.types.user_drop(value_ty);
                        let boxed = self.ir.exprs.alloc(IrExpr::NativeState {
                            value,
                            type_id,
                            drop_glue,
                            ty: Type::INT,
                        });
                        let token = self.ir.exprs.alloc(IrExpr::NativeUserData {
                            state: boxed,
                            borrowed: false,
                            ty: Type::RawPtr,
                        });
                        // The token is a pointer word; a queue slot is an
                        // `Int`. The two are the same bits, and the VM is the
                        // engine that says so out loud — it carries the value's
                        // kind beside it and refuses one where the other
                        // belongs, where native sees one machine word either
                        // way.
                        self.ir.exprs.alloc(IrExpr::Convert {
                            operand: token,
                            kind: kira_semantics_model::hir::ConvertKind::RawPtrToInt,
                            ty: Type::INT,
                        })
                    }
                };
                return self.channel_op(ChannelPrim::Send, vec![sender, value]);
            }
            HirExpr::ChannelReceive {
                receiver,
                payload,
                wire,
                failure,
                ty,
            } => return self.lower_channel_receive(receiver, payload, wire, failure, ty),
            HirExpr::ChannelClose { end, sender, wire } => {
                let end = self.lower_expr(end);
                // A receiver closing discards whatever is still queued. When
                // those slots hold tokens they own the storage behind them, so
                // the queue is drained and released before the end is closed
                // rather than dropped on the floor. A sender closing discards
                // nothing — the queue stays for the receiver to drain.
                if !sender && wire.is_boxed() {
                    self.uses_tasks = true;
                    let callee =
                        self.task_base + crate::tasks::TaskFns::COUNT + crate::channels::CLOSER;
                    return self.ir.exprs.alloc(IrExpr::Call {
                        callee: IrCallee::User(callee),
                        args: vec![end],
                        result: Type::Void,
                        writebacks: Vec::new(),
                    });
                }
                let prim = match sender {
                    true => ChannelPrim::CloseSender,
                    false => ChannelPrim::CloseReceiver,
                };
                return self.channel_op(prim, vec![end]);
            }
            HirExpr::TaskDetach { handle } => {
                return self.lower_task_handle_call(handle, crate::tasks::TaskFns::DETACH);
            }
            HirExpr::TaskCancel { handle } => {
                return self.lower_task_handle_call(handle, crate::tasks::TaskFns::CANCEL);
            }
        };
        self.ir.exprs.alloc(node)
    }
}
