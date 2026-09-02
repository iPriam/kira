//! Concrete payload arms for dynamic construct and trait dispatch.
//!
//! A mutating arm owns a temporary concrete payload, invokes the selected
//! implementation through its ordinary mutable-receiver ABI, and rebuilds the
//! erased enum with the updated payload. The enclosing selector then writes
//! that enum back through its own receiver place.

use kira_semantics_model::hir::{CallableSignature, Callee, FuncId, HirExpr, HirFunction, HirPlace, HirStmt, HirStmtId, HirWriteback, LocalId,};
use kira_semantics_model::{EnumId, OwnershipMode, Type};
use kira_source::Span;

use super::{ConstructVariant, DispatchMethod};
use crate::analyze::{Analyzer, FnCtx};

/// The pieces one dispatcher branch forwards to a concrete method.
struct DispatchArm<'locals> {
    target: FuncId,
    /// What the concrete method returns, which a child family may have made
    /// more specific than the dispatcher's own result.
    target_result: Type,
    receiver: LocalId,
    family: EnumId,
    variant: ConstructVariant,
    param_locals: &'locals [LocalId],
    params: &'locals [Type],
    result: Type,
    mutates_self: bool,
}

impl Analyzer<'_> {
    /// Builds one concrete implementation arm in its own native function.
    ///
    /// The family dispatcher stays as a small tag test, while this helper owns
    /// the payload extraction and the call into the concrete implementation.
    /// The VM sees the same HIR operation sequence and therefore keeps the
    /// existing semantics; native code simply gets a real call boundary for
    /// stack-frame sizing.
    pub(crate) fn construct_dispatch_arm_function(
        &mut self,
        dispatch: DispatchMethod<'_>,
        variant: ConstructVariant,
        target: FuncId,
        target_result: Type,
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
        let body = self.construct_dispatch_arm(
            &mut ctx,
            DispatchArm {
                target,
                target_result,
                receiver,
                family: family_id,
                variant,
                param_locals: &param_locals,
                params,
                result,
                mutates_self,
            },
        );
        HirFunction {
            name: format!("Any {family}.{method}$dispatch_arm{}", variant.tag),
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

    /// Finds the concrete member selected by one family variant.
    pub(super) fn construct_method_target(
        &self,
        variant: ConstructVariant,
        method: &str,
    ) -> Option<(FuncId, Type)> {
        let owner = self.member_owner_name(variant.ty);
        self.lookup_function(&format!("{owner}.{method}"))
            .map(|(id, _, result)| (id, result))
    }

    fn construct_dispatch_arm(&mut self, ctx: &mut FnCtx, arm: DispatchArm<'_>) -> Vec<HirStmtId> {
        let DispatchArm {
            target,
            target_result,
            receiver,
            mutates_self,
            family,
            variant,
            param_locals,
            params,
            result,
        } = arm;
        let family_value = self.program.exprs.alloc(HirExpr::Local {
            local: receiver,
            ty: Type::Enum(family),
        });
        let concrete_ty = variant.ty;
        let concrete = self.program.exprs.alloc(HirExpr::EnumPayload {
            value: family_value,
            ty: concrete_ty,
        });
        let concrete_local =
            mutates_self.then(|| ctx.declare_hidden_as(concrete_ty, true, OwnershipMode::Owned));
        let receiver_arg = concrete_local
            .map(|local| {
                self.program.exprs.alloc(HirExpr::Local {
                    local,
                    ty: concrete_ty,
                })
            })
            .unwrap_or(concrete);
        let mut args = vec![receiver_arg];
        for (&local, &ty) in param_locals.iter().zip(params.iter()) {
            args.push(self.program.exprs.alloc(HirExpr::Local { local, ty }));
        }
        let call = self.program.exprs.alloc(HirExpr::Call {
            callee: Callee::User(target),
            args,
            ty: target_result,
            writebacks: concrete_local
                .map(|local| {
                    vec![HirWriteback {
                        param: 0,
                        place: HirPlace {
                            local,
                            path: Vec::new(),
                        },
                    }]
                })
                .unwrap_or_default(),
        });
        // A child family may return something more specific than the family
        // this dispatcher belongs to promised, so the arm carries its answer
        // up to the declared result rather than relabelling it.
        let call = self.coerce_into(call, result);
        let mut body = Vec::new();
        if let Some(local) = concrete_local {
            body.push(self.program.stmts.alloc(HirStmt::Let {
                local,
                init: concrete,
            }));
        }
        if result == Type::Void {
            body.push(self.program.stmts.alloc(HirStmt::Expr { expr: call }));
        } else {
            let result_local = ctx.declare_hidden_as(result, false, OwnershipMode::Owned);
            body.push(self.program.stmts.alloc(HirStmt::Let {
                local: result_local,
                init: call,
            }));
            if let Some(local) = concrete_local {
                let updated = self.program.exprs.alloc(HirExpr::Local {
                    local,
                    ty: concrete_ty,
                });
                let replacement = self.program.exprs.alloc(HirExpr::EnumNew {
                    enum_id: family,
                    tag: variant.tag,
                    payload: Some(updated),
                });
                body.push(self.program.stmts.alloc(HirStmt::Assign {
                    place: HirPlace {
                        local: receiver,
                        path: Vec::new(),
                    },
                    value: replacement,
                }));
            }
            body.push(self.program.stmts.alloc(HirStmt::Return {
                value: Some(self.program.exprs.alloc(HirExpr::Local {
                    local: result_local,
                    ty: result,
                })),
            }));
            return body;
        }
        if let Some(local) = concrete_local {
            let updated = self.program.exprs.alloc(HirExpr::Local {
                local,
                ty: concrete_ty,
            });
            let replacement = self.program.exprs.alloc(HirExpr::EnumNew {
                enum_id: family,
                tag: variant.tag,
                payload: Some(updated),
            });
            body.push(self.program.stmts.alloc(HirStmt::Assign {
                place: HirPlace {
                    local: receiver,
                    path: Vec::new(),
                },
                value: replacement,
            }));
        }
        body.push(self.program.stmts.alloc(HirStmt::Return { value: None }));
        body
    }
}
