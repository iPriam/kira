use kira_semantics_model::Type;
use kira_semantics_model::hir::{HirBinaryOp, HirExpr, HirExprId, HirStmtId, LocalId};
use kira_source::Span;
use kira_syntax_model::ast::{BinaryOp, ExprId, MatchArm, MatchPattern};

use crate::analyze::{Analyzer, FnCtx};
use crate::operators::resolve_binary;

use super::{ResolvedArm, arm_is_wildcard};

#[derive(Default)]
pub(crate) struct ScalarCoverage {
    values: Vec<ScalarKey>,
    pub(crate) bools: [bool; 2],
}

impl Analyzer<'_> {
    /// Analyzes a `match` over an `Int`, `Bool`, or `String` subject.
    pub(super) fn analyze_scalar_match(
        &mut self,
        ctx: &mut FnCtx,
        slot: LocalId,
        subject_ty: Type,
        arms: &[MatchArm],
        span: Span,
        out: &mut Vec<HirStmtId>,
    ) {
        let mut branch = crate::ownership::BranchMoves::start(ctx);
        let mut resolved: Vec<ResolvedArm> = Vec::with_capacity(arms.len());
        let mut coverage = ScalarCoverage {
            values: Vec::new(),
            bools: [false, false],
        };
        let mut wildcard = false;
        let mut clean = true;

        for arm in arms {
            if wildcard {
                self.emit(
                    arm.span,
                    "KSEM381",
                    "an `else` arm must be the last arm; the arms after it can never run",
                );
                clean = false;
                continue;
            }
            if arm_is_wildcard(arm) {
                wildcard = true;
                branch.enter_arm(ctx);
                let body = self.analyze_block(ctx, &arm.body);
                branch.leave_arm(ctx, self.body_definitely_returns(&body));
                resolved.push(ResolvedArm { test: None, body });
                continue;
            }
            let Some(test) = self.resolve_scalar_arm(ctx, slot, subject_ty, arm, &mut coverage)
            else {
                clean = false;
                continue;
            };
            branch.enter_arm(ctx);
            let body = {
                ctx.push_scope();
                let body = self.analyze_block(ctx, &arm.body);
                ctx.pop_scope();
                body
            };
            branch.leave_arm(ctx, self.body_definitely_returns(&body));
            resolved.push(ResolvedArm {
                test: Some(test),
                body,
            });
        }

        let exhaustive =
            self.check_scalar_coverage(subject_ty, wildcard, coverage.bools, clean, span);
        branch.finish(ctx, exhaustive);
        out.extend(self.build_chain(resolved));
    }

    /// Resolves one non-wildcard scalar arm to its selecting test, reporting a
    /// pattern of the wrong type, a duplicate value, and a range on a
    /// non-integer subject.
    pub(crate) fn resolve_scalar_arm(
        &mut self,
        ctx: &mut FnCtx,
        slot: LocalId,
        subject_ty: Type,
        arm: &MatchArm,
        coverage: &mut ScalarCoverage,
    ) -> Option<HirExprId> {
        let mut tests: Vec<HirExprId> = Vec::new();
        for pattern in &arm.patterns {
            match pattern {
                MatchPattern::Value { expr, span } => {
                    if let Some(test) =
                        self.resolve_value_pattern(ctx, slot, subject_ty, *expr, *span, coverage)
                    {
                        tests.push(test);
                    }
                }
                MatchPattern::Range { start, end, span } => {
                    if let Some(test) =
                        self.resolve_range_pattern(ctx, slot, subject_ty, *start, *end, *span)
                    {
                        tests.push(test);
                    }
                }
                MatchPattern::Variant { variant_span, .. } => {
                    self.emit(
                        *variant_span,
                        "KSEM383",
                        format!(
                            "a `match` on `{}` takes literal and range patterns, not a variant \
                             name",
                            self.type_name(subject_ty)
                        ),
                    );
                }
                MatchPattern::Wildcard { .. } => {}
            }
        }
        if tests.is_empty() {
            return None;
        }
        self.any_of(tests)
    }

    /// Resolves a literal pattern to `subject == literal`.
    fn resolve_value_pattern(
        &mut self,
        ctx: &mut FnCtx,
        slot: LocalId,
        subject_ty: Type,
        expr: ExprId,
        span: Span,
        coverage: &mut ScalarCoverage,
    ) -> Option<HirExprId> {
        let literal = self.analyze_expr_expecting(ctx, expr, Some(subject_ty));
        let literal_ty = self.program.expr(literal).type_of();
        if literal_ty == Type::Error {
            return None;
        }
        // The literal is compared with `==`, so its type must be one `==`
        // accepts against the subject — which is exactly the subject's type,
        // with a bare literal adapting to a width.
        let Some((op, _)) = resolve_binary(BinaryOp::Eq, subject_ty, literal_ty) else {
            self.emit(
                span,
                "KSEM383",
                format!(
                    "a `match` pattern of type `{}` does not match the subject's `{}`",
                    self.type_name(literal_ty),
                    self.type_name(subject_ty)
                ),
            );
            return None;
        };
        if let Some(key) = self.scalar_key(literal) {
            if coverage.values.contains(&key) {
                self.emit(span, "KSEM382", "this value is matched by an earlier arm");
                return None;
            }
            if let ScalarKey::Bool(value) = key {
                coverage.bools[usize::from(value)] = true;
            }
            coverage.values.push(key);
        }
        let read = self.program.exprs.alloc(HirExpr::Local {
            local: slot,
            ty: subject_ty,
        });
        Some(self.program.exprs.alloc(HirExpr::Binary {
            op,
            lhs: read,
            rhs: literal,
            ty: Type::Bool,
        }))
    }

    /// Resolves a range pattern to `subject >= start && subject < end`.
    fn resolve_range_pattern(
        &mut self,
        ctx: &mut FnCtx,
        slot: LocalId,
        subject_ty: Type,
        start: ExprId,
        end: ExprId,
        span: Span,
    ) -> Option<HirExprId> {
        if !matches!(subject_ty, Type::Int(_)) {
            self.emit(
                span,
                "KSEM383",
                format!(
                    "a range pattern matches an integer, not `{}`",
                    self.type_name(subject_ty)
                ),
            );
            return None;
        }
        let start_hir = self.analyze_expr_expecting(ctx, start, Some(subject_ty));
        let end_hir = self.analyze_expr_expecting(ctx, end, Some(subject_ty));
        let start_ty = self.program.expr(start_hir).type_of();
        let end_ty = self.program.expr(end_hir).type_of();
        if start_ty == Type::Error || end_ty == Type::Error {
            return None;
        }
        let (ge, _) = resolve_binary(BinaryOp::Ge, subject_ty, start_ty)?;
        let (lt, _) = resolve_binary(BinaryOp::Lt, subject_ty, end_ty)?;
        let read_low = self.program.exprs.alloc(HirExpr::Local {
            local: slot,
            ty: subject_ty,
        });
        let read_high = self.program.exprs.alloc(HirExpr::Local {
            local: slot,
            ty: subject_ty,
        });
        let low = self.program.exprs.alloc(HirExpr::Binary {
            op: ge,
            lhs: read_low,
            rhs: start_hir,
            ty: Type::Bool,
        });
        let high = self.program.exprs.alloc(HirExpr::Binary {
            op: lt,
            lhs: read_high,
            rhs: end_hir,
            ty: Type::Bool,
        });
        Some(self.program.exprs.alloc(HirExpr::Binary {
            op: HirBinaryOp::And,
            lhs: low,
            rhs: high,
            ty: Type::Bool,
        }))
    }

    /// A comparable key for a scalar literal, for duplicate detection. `None`
    /// when the literal is not a constant this can compare (a `String` from a
    /// non-literal, say), which simply skips the check.
    fn scalar_key(&self, literal: HirExprId) -> Option<ScalarKey> {
        match self.program.expr(literal) {
            HirExpr::Int(value) => Some(ScalarKey::Int(*value)),
            HirExpr::Bool(value) => Some(ScalarKey::Bool(*value)),
            HirExpr::Str(value) => Some(ScalarKey::Str(value.clone())),
            _ => None,
        }
    }

    /// Whether a scalar `match` is exhaustive: an `else` arm, or a `Bool`
    /// subject with both `true` and `false` covered. `Int` and `String` cannot
    /// be listed exhaustively, so they must end with `else`.
    pub(crate) fn check_scalar_coverage(
        &mut self,
        subject_ty: Type,
        wildcard: bool,
        seen_bool: [bool; 2],
        clean: bool,
        span: Span,
    ) -> bool {
        if !clean {
            return false;
        }
        if wildcard {
            return true;
        }
        if subject_ty == Type::Bool && seen_bool[0] && seen_bool[1] {
            return true;
        }
        self.emit(
            span,
            "KSEM380",
            format!(
                "a `match` on `{}` must end with an `else` arm; its values cannot be listed \
                 exhaustively",
                self.type_name(subject_ty)
            ),
        );
        false
    }
}

/// A resolved scalar literal, for detecting a value matched twice.
#[derive(PartialEq)]
pub(crate) enum ScalarKey {
    Int(i64),
    Bool(bool),
    Str(String),
}
