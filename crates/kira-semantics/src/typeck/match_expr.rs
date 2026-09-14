//! Expression-valued `match`.
//!
//! The source arm syntax is shared with statement `match`, but an expression
//! arm ends in an expression whose value is selected. Earlier statements in a
//! braced arm become branch-local setup on `HirExpr::Select`, so payload
//! bindings, local calculations, and side effects stay lazy.

use kira_semantics_model::hir::{HirExpr, HirExprId, HirStmt, HirStmtId, LocalId};
use kira_semantics_model::{EnumId, Type};
use kira_source::Span;
use kira_syntax_model::ast::{Block, ExprId, MatchArm, MatchBinding, Stmt};

use crate::analyze::{Analyzer, FnCtx};
use crate::operators::unify_branches;
use crate::ownership::BranchMoves;
use crate::stmt::matches::{EnumArmResolution, EnumSubject, ScalarCoverage, arm_is_wildcard};

struct ValueArm {
    test: Option<HirExprId>,
    setup: Vec<HirStmtId>,
    value: HirExprId,
    cleanup: Vec<LocalId>,
    ty: Type,
}

impl Analyzer<'_> {
    /// Type-checks `match subject { pattern -> value ... }` as a value.
    pub(crate) fn analyze_match_expression(
        &mut self,
        ctx: &mut FnCtx,
        subject: ExprId,
        arms: &[MatchArm],
        span: Span,
        expected: Option<Type>,
    ) -> HirExprId {
        let subject_span = self.tree.expr(subject).span();
        let subject_value = self.analyze_expr(ctx, subject);
        let subject_ty = self.program.expr(subject_value).type_of();
        let subject_slot = ctx.declare_hidden(subject_ty, false);
        let mut prologue = vec![self.program.stmts.alloc(HirStmt::Let {
            local: subject_slot,
            init: subject_value,
        })];

        let resolved = match subject_ty {
            Type::Enum(enum_id) => self.analyze_enum_value_arms(
                ctx,
                EnumSubject {
                    id: enum_id,
                    slot: subject_slot,
                },
                arms,
                span,
                expected,
                &mut prologue,
            ),
            ty if self.is_scalar_match_subject(ty) => {
                self.analyze_scalar_value_arms(ctx, subject_slot, ty, arms, span, expected)
            }
            Type::Error => Vec::new(),
            other => {
                self.emit(
                    subject_span,
                    "KSEM125",
                    format!(
                        "a `match` subject must be an enum, an `Int`, a `Bool`, or a `String`, found `{}`",
                        self.type_name(other)
                    ),
                );
                self.analyze_invalid_value_arms(ctx, arms, expected);
                Vec::new()
            }
        };

        for stmt in prologue {
            ctx.hoist_stmt(stmt);
        }
        self.finish_value_match(ctx, resolved, span)
    }

    fn analyze_enum_value_arms(
        &mut self,
        ctx: &mut FnCtx,
        subject: EnumSubject,
        arms: &[MatchArm],
        span: Span,
        expected: Option<Type>,
        prologue: &mut Vec<HirStmtId>,
    ) -> Vec<ValueArm> {
        let EnumSubject {
            id: enum_id,
            slot: subject_slot,
        } = subject;
        let subject_read = self.program.exprs.alloc(HirExpr::Local {
            local: subject_slot,
            ty: Type::Enum(enum_id),
        });
        let tag = self.program.exprs.alloc(HirExpr::EnumTag {
            value: subject_read,
        });
        let tag_slot = ctx.declare_hidden(Type::INT, false);
        prologue.push(self.program.stmts.alloc(HirStmt::Let {
            local: tag_slot,
            init: tag,
        }));

        let mut branch = BranchMoves::start(ctx);
        let mut covered = Vec::new();
        let mut wildcard = false;
        let mut clean = true;
        let mut values = Vec::with_capacity(arms.len());
        let mut inferred = expected;

        for arm in arms {
            if wildcard {
                self.emit(
                    arm.span,
                    "KSEM381",
                    "an `else` arm must be the last arm; the arms after it can never run",
                );
                clean = false;
                self.analyze_invalid_value_arm(ctx, &arm.body, inferred);
                continue;
            }

            let (test, binding) = if arm_is_wildcard(arm) {
                wildcard = true;
                (None, None)
            } else {
                match self.resolve_enum_arm(enum_id, tag_slot, arm, &mut covered) {
                    Some(EnumArmResolution { test, binding }) => (test, binding),
                    None => {
                        clean = false;
                        self.analyze_invalid_value_arm(ctx, &arm.body, inferred);
                        continue;
                    }
                }
            };

            branch.enter_arm(ctx);
            let (setup, value, cleanup, invalid) = self.analyze_value_arm(
                ctx,
                &arm.body,
                inferred,
                binding.map(|(binding, tag)| (enum_id, subject_slot, binding, tag)),
            );
            let ty = self.program.expr(value).type_of();
            if inferred.is_none() && ty != Type::Error {
                inferred = Some(ty);
            }
            clean &= !invalid && ty != Type::Error;
            branch.leave_arm(ctx, false);
            values.push(ValueArm {
                test,
                setup,
                value,
                cleanup,
                ty,
            });
        }

        let exhaustive = self.check_enum_coverage(enum_id, &covered, wildcard, clean, span);
        branch.finish(ctx, exhaustive);
        values
    }

    fn analyze_scalar_value_arms(
        &mut self,
        ctx: &mut FnCtx,
        subject_slot: LocalId,
        subject_ty: Type,
        arms: &[MatchArm],
        span: Span,
        expected: Option<Type>,
    ) -> Vec<ValueArm> {
        let mut branch = BranchMoves::start(ctx);
        let mut coverage = ScalarCoverage::default();
        let mut wildcard = false;
        let mut clean = true;
        let mut values = Vec::with_capacity(arms.len());
        let mut inferred = expected;

        for arm in arms {
            if wildcard {
                self.emit(
                    arm.span,
                    "KSEM381",
                    "an `else` arm must be the last arm; the arms after it can never run",
                );
                clean = false;
                self.analyze_invalid_value_arm(ctx, &arm.body, inferred);
                continue;
            }
            let test = if arm_is_wildcard(arm) {
                wildcard = true;
                None
            } else {
                match self.resolve_scalar_arm(ctx, subject_slot, subject_ty, arm, &mut coverage) {
                    Some(test) => Some(test),
                    None => {
                        clean = false;
                        self.analyze_invalid_value_arm(ctx, &arm.body, inferred);
                        continue;
                    }
                }
            };

            branch.enter_arm(ctx);
            let (setup, value, cleanup, invalid) =
                self.analyze_value_arm(ctx, &arm.body, inferred, None);
            let ty = self.program.expr(value).type_of();
            if inferred.is_none() && ty != Type::Error {
                inferred = Some(ty);
            }
            clean &= !invalid && ty != Type::Error;
            branch.leave_arm(ctx, false);
            values.push(ValueArm {
                test,
                setup,
                value,
                cleanup,
                ty,
            });
        }

        let exhaustive =
            self.check_scalar_coverage(subject_ty, wildcard, coverage.bools, clean, span);
        branch.finish(ctx, exhaustive);
        values
    }

    fn analyze_value_arm(
        &mut self,
        ctx: &mut FnCtx,
        block: &Block,
        expected: Option<Type>,
        payload: Option<(EnumId, LocalId, MatchBinding, u32)>,
    ) -> (Vec<HirStmtId>, HirExprId, Vec<LocalId>, bool) {
        let first_local = ctx.locals.len() as u32;
        ctx.push_scope();
        let mut setup = Vec::new();
        if let Some((enum_id, subject_slot, binding, tag)) = payload {
            self.bind_enum_payload(ctx, enum_id, subject_slot, binding, tag, &mut setup);
        }

        let Some((&last, prefix)) = block.stmts.split_last() else {
            self.emit(
                block.span,
                "KSEM386",
                "a `match` expression arm must produce a value",
            );
            let cleanup = (first_local..ctx.locals.len() as u32)
                .map(LocalId)
                .collect();
            ctx.pop_scope();
            return (
                setup,
                self.program.exprs.alloc(HirExpr::Error),
                cleanup,
                true,
            );
        };
        for &stmt in prefix {
            self.analyze_stmt(ctx, stmt, &mut setup);
        }
        let diverges = self.body_definitely_returns(&setup);

        let value = match self.tree.stmt(last).clone() {
            Stmt::Expr { expr, .. } => {
                let value = self.analyze_expr_expecting(ctx, expr, expected);
                setup.extend(ctx.take_pending_stmts());
                let deferred = ctx.take_deferred_stmts();
                if !deferred.is_empty() {
                    self.emit(
                        self.tree.expr(expr).span(),
                        "KSEM387",
                        "a `match` arm value cannot defer a writeback after its value",
                    );
                }
                value
            }
            _ => {
                self.analyze_stmt(ctx, last, &mut setup);
                self.emit(
                    block.span,
                    "KSEM386",
                    "a `match` expression arm must end with an expression value",
                );
                self.program.exprs.alloc(HirExpr::Error)
            }
        };
        if diverges {
            self.emit(
                block.span,
                "KSEM386",
                "a `match` expression arm cannot return before producing its value",
            );
        }
        let cleanup = (first_local..ctx.locals.len() as u32)
            .map(LocalId)
            .collect();
        ctx.pop_scope();
        (setup, value, cleanup, diverges)
    }

    fn analyze_invalid_value_arms(
        &mut self,
        ctx: &mut FnCtx,
        arms: &[MatchArm],
        expected: Option<Type>,
    ) {
        for arm in arms {
            self.analyze_invalid_value_arm(ctx, &arm.body, expected);
        }
    }

    fn analyze_invalid_value_arm(
        &mut self,
        ctx: &mut FnCtx,
        block: &Block,
        expected: Option<Type>,
    ) {
        let _ = self.analyze_value_arm(ctx, block, expected, None);
    }

    fn finish_value_match(
        &mut self,
        ctx: &mut FnCtx,
        mut arms: Vec<ValueArm>,
        span: Span,
    ) -> HirExprId {
        let Some(last) = arms.pop() else {
            return self.program.exprs.alloc(HirExpr::Error);
        };
        let mut ty = last.ty;
        for arm in &arms {
            let Some(joined) = unify_branches(arm.ty, ty) else {
                self.emit(
                    span,
                    "KSEM388",
                    format!(
                        "the arms of a `match` expression disagree: `{}` and `{}`",
                        self.type_name(arm.ty),
                        self.type_name(ty)
                    ),
                );
                return self.program.exprs.alloc(HirExpr::Error);
            };
            ty = joined;
        }
        if ty == Type::Void || ty == Type::Error {
            if ty == Type::Void {
                self.emit(span, "KSEM386", "a `match` expression must produce a value");
            }
            return self.program.exprs.alloc(HirExpr::Error);
        }

        let mut tail_setup = last.setup;
        let mut tail_cleanup = last.cleanup;
        let mut tail = self.coerce_into(last.value, ty);
        for arm in arms.into_iter().rev() {
            let Some(cond) = arm.test else {
                continue;
            };
            let then = self.coerce_into(arm.value, ty);
            tail = self.program.exprs.alloc(HirExpr::Select {
                cond,
                then_setup: arm.setup,
                then,
                then_cleanup: arm.cleanup,
                otherwise_setup: tail_setup,
                otherwise: tail,
                otherwise_cleanup: tail_cleanup,
                ty,
            });
            tail_setup = Vec::new();
            tail_cleanup = Vec::new();
        }
        // A single unconditional arm has no branch node to own its cleanup.
        // Its setup still hoists before the expression; the cleanup is carried
        // by a degenerate always-true select so it runs only after the value is
        // materialized, exactly like every other arm.
        if !tail_cleanup.is_empty() {
            let yes = self.program.exprs.alloc(HirExpr::Bool(true));
            let unreachable = self.program.exprs.alloc(HirExpr::Error);
            tail = self.program.exprs.alloc(HirExpr::Select {
                cond: yes,
                then_setup: tail_setup,
                then: tail,
                then_cleanup: tail_cleanup,
                otherwise_setup: Vec::new(),
                otherwise: unreachable,
                otherwise_cleanup: Vec::new(),
                ty,
            });
            tail_setup = Vec::new();
        }
        for stmt in tail_setup {
            ctx.hoist_stmt(stmt);
        }
        tail
    }
}
