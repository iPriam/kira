//! Balanced tag-tree functions for construct-family and trait dispatch.
//!
//! A selector only reads the erased receiver's tag and forwards the same
//! receiver and arguments to a child. Keeping the tree separate from payload
//! arms makes the frame that performs selection independent of the concrete
//! aggregate each variant carries.

use kira_semantics_model::hir::{CallableSignature, Callee, FuncId, HirBinaryOp, HirExpr, HirFunction, HirPlace, HirStmt, HirStmtId, HirWriteback,
    LocalId,};
use kira_semantics_model::{EnumId, OwnershipMode, Type};
use kira_source::Span;

use super::{ConstructVariant, DispatchMethod};
use crate::analyze::{Analyzer, FnCtx};

/// The values one dispatch-tree node forwards to its selected child.
struct DispatchCall<'a> {
    /// The child function to call.
    callee: FuncId,
    /// The receiver local in this tree node.
    receiver: LocalId,
    /// The synthesized enum carrying the receiver.
    family: EnumId,
    /// Parameter locals forwarded after the receiver.
    param_locals: &'a [LocalId],
    /// Parameter types forwarded after the receiver.
    params: &'a [Type],
    /// The result type of the call.
    result: Type,
    /// Whether the receiver is written back after the child returns.
    mutates_self: bool,
}

impl Analyzer<'_> {
    /// Builds one node in a balanced tag selector tree.
    pub(crate) fn construct_dispatch_tree_function(
        &mut self,
        dispatch: DispatchMethod<'_>,
        arms: &[(ConstructVariant, FuncId)],
        fallback: FuncId,
        name: String,
        tree_number: &mut u32,
    ) -> HirFunction {
        let DispatchMethod {
            family,
            method,
            family_id,
            params,
            result,
            mutates_self,
        } = dispatch;
        let mut ctx = FnCtx::new(result);
        // Dispatch only inspects the erased value and forwards the call. Keep
        // the caller's value borrowed so every node can lend the same storage;
        // making this an owned parameter would clone/drop the full enum at
        // every branch in a native executable.
        let receiver = ctx.declare_hidden_as(
            Type::Enum(family_id),
            mutates_self,
            if mutates_self {
                OwnershipMode::BorrowMut
            } else {
                OwnershipMode::BorrowRead
            },
        );
        let param_locals: Vec<_> = params
            .iter()
            .map(|&ty| ctx.declare_hidden_as(ty, false, OwnershipMode::BorrowRead))
            .collect();

        let body = if arms.len() == 1 {
            let (variant, arm) = arms[0];
            let value = self.program.exprs.alloc(HirExpr::Local {
                local: receiver,
                ty: Type::Enum(family_id),
            });
            let tag = self.program.exprs.alloc(HirExpr::EnumTag { value });
            let wanted = self
                .program
                .exprs
                .alloc(HirExpr::Int(i64::from(variant.tag)));
            let cond = self.program.exprs.alloc(HirExpr::Binary {
                op: HirBinaryOp::EqInt,
                lhs: tag,
                rhs: wanted,
                ty: Type::Bool,
            });
            let then_body = self.construct_dispatch_function_call(DispatchCall {
                callee: arm,
                receiver,
                family: family_id,
                param_locals: &param_locals,
                params,
                result,
                mutates_self,
            });
            let else_body = if arm == fallback {
                then_body.clone()
            } else {
                self.construct_dispatch_function_call(DispatchCall {
                    callee: fallback,
                    receiver,
                    family: family_id,
                    param_locals: &param_locals,
                    params,
                    result,
                    mutates_self,
                })
            };
            vec![self.program.stmts.alloc(HirStmt::If {
                cond,
                then_body,
                else_body,
            })]
        } else {
            let middle = arms.len() / 2;
            let pivot = arms[middle].0.tag;
            let left_id = self.reserve_synth();
            let left_number = *tree_number;
            *tree_number += 1;
            let left = self.construct_dispatch_tree_function(
                dispatch,
                &arms[..middle],
                fallback,
                format!("Any {family}.{method}$dispatch_tree{left_number}"),
                tree_number,
            );
            self.fill_synth(left_id, left);
            let right_id = self.reserve_synth();
            let right_number = *tree_number;
            *tree_number += 1;
            let right = self.construct_dispatch_tree_function(
                dispatch,
                &arms[middle..],
                fallback,
                format!("Any {family}.{method}$dispatch_tree{right_number}"),
                tree_number,
            );
            self.fill_synth(right_id, right);

            let value = self.program.exprs.alloc(HirExpr::Local {
                local: receiver,
                ty: Type::Enum(family_id),
            });
            let tag = self.program.exprs.alloc(HirExpr::EnumTag { value });
            let pivot = self.program.exprs.alloc(HirExpr::Int(i64::from(pivot)));
            let cond = self.program.exprs.alloc(HirExpr::Binary {
                op: HirBinaryOp::LtInt,
                lhs: tag,
                rhs: pivot,
                ty: Type::Bool,
            });
            let then_body = self.construct_dispatch_function_call(DispatchCall {
                callee: left_id,
                receiver,
                family: family_id,
                param_locals: &param_locals,
                params,
                result,
                mutates_self,
            });
            let else_body = self.construct_dispatch_function_call(DispatchCall {
                callee: right_id,
                receiver,
                family: family_id,
                param_locals: &param_locals,
                params,
                result,
                mutates_self,
            });
            vec![self.program.stmts.alloc(HirStmt::If {
                cond,
                then_body,
                else_body,
            })]
        };
        HirFunction {
            name,
            param_count: 1 + params.len() as u32,
            return_type: result,
            locals: ctx.locals,
            body,
            is_main: false,
            is_main_thread: false,
            is_async: false,
            execution: kira_semantics_model::Execution::Inherited,
            mutates_self,
            name_span: Span::new(0, 0),
            signature: CallableSignature::synthesized(&[], result),
        }
    }

    /// Emits the small call/return body for one dispatcher branch.
    fn construct_dispatch_function_call(&mut self, call: DispatchCall<'_>) -> Vec<HirStmtId> {
        let DispatchCall {
            callee,
            receiver,
            family,
            param_locals,
            params,
            result,
            mutates_self,
        } = call;
        let mut args = vec![self.program.exprs.alloc(HirExpr::Local {
            local: receiver,
            ty: Type::Enum(family),
        })];
        for (&local, &ty) in param_locals.iter().zip(params.iter()) {
            args.push(self.program.exprs.alloc(HirExpr::Local { local, ty }));
        }
        let call = self.program.exprs.alloc(HirExpr::Call {
            callee: Callee::User(callee),
            args,
            ty: result,
            writebacks: if mutates_self {
                vec![HirWriteback {
                    param: 0,
                    place: HirPlace {
                        local: receiver,
                        path: Vec::new(),
                    },
                }]
            } else {
                Vec::new()
            },
        });
        if result == Type::Void {
            vec![
                self.program.stmts.alloc(HirStmt::Expr { expr: call }),
                self.program.stmts.alloc(HirStmt::Return { value: None }),
            ]
        } else {
            vec![
                self.program
                    .stmts
                    .alloc(HirStmt::Return { value: Some(call) }),
            ]
        }
    }
}
