//! `cond ? then : otherwise`, the one expression in the language that is
//! control flow.
//!
//! Exactly one branch is evaluated, which is why it cannot be desugared into a
//! call and why every backend lowers it as a branch rather than a select.

use kira_semantics_model::Type;
use kira_semantics_model::hir::{HirExpr, HirExprId};
use kira_syntax_model::ast::{Expr, ExprId};

use crate::analyze::{Analyzer, FnCtx};
use crate::operators::unify_branches;

impl Analyzer<'_> {
    /// Type-checks `cond ? then : otherwise`.
    ///
    /// The expected-type hint is forwarded to both branches so an empty array
    /// literal keeps working in either one — `flag ? [] : xs` needs the hint to
    /// reach the `[]` exactly as `var xs: [Int] = []` does. A leading-dot member
    /// resolves the same way: whichever branch types concretely anchors the
    /// other, so `flag ? .Red : tone` and `flag ? tone : .Red` both work.
    pub(super) fn analyze_conditional(
        &mut self,
        ctx: &mut FnCtx,
        cond: ExprId,
        then: ExprId,
        otherwise: ExprId,
        span: kira_source::Span,
        expected: Option<Type>,
    ) -> HirExprId {
        let cond_hir = self.analyze_expr(ctx, cond);
        let cond_ty = self.program.expr(cond_hir).type_of();
        if cond_ty != Type::Error && cond_ty != Type::Bool {
            let name = self.type_name(cond_ty);
            self.emit(
                self.tree.expr(cond).span(),
                "KSEM131",
                format!("the condition of `? :` must be `Bool`, not `{name}`"),
            );
            return self.program.exprs.alloc(HirExpr::Error);
        }

        // As in `analyze_binary`, the concrete branch is analyzed first when the
        // other is a leading dot, so the dot inherits a type to resolve against.
        let then_is_dot = matches!(self.tree.expr(then), Expr::DotMember { .. });
        let otherwise_is_dot = matches!(self.tree.expr(otherwise), Expr::DotMember { .. });
        let (then_hir, otherwise_hir) = if then_is_dot && !otherwise_is_dot {
            let otherwise_hir = self.analyze_expr_expecting(ctx, otherwise, expected);
            let hint = self.program.expr(otherwise_hir).type_of();
            let then_hir = self.analyze_expr_expecting(ctx, then, Some(hint));
            (then_hir, otherwise_hir)
        } else {
            let then_hir = self.analyze_expr_expecting(ctx, then, expected);
            let hint = self.program.expr(then_hir).type_of();
            let otherwise_hir = if otherwise_is_dot {
                self.analyze_expr_expecting(ctx, otherwise, Some(hint))
            } else {
                self.analyze_expr_expecting(ctx, otherwise, expected.or(Some(hint)))
            };
            (then_hir, otherwise_hir)
        };

        let then_ty = self.program.expr(then_hir).type_of();
        let otherwise_ty = self.program.expr(otherwise_hir).type_of();
        if then_ty == Type::Error || otherwise_ty == Type::Error {
            return self.program.exprs.alloc(HirExpr::Error);
        }

        let Some(ty) = unify_branches(then_ty, otherwise_ty) else {
            let (then_name, otherwise_name) =
                (self.type_name(then_ty), self.type_name(otherwise_ty));
            self.emit(
                span,
                "KSEM132",
                format!("the branches of `? :` disagree: `{then_name}` and `{otherwise_name}`"),
            );
            return self.program.exprs.alloc(HirExpr::Error);
        };

        // A `? :` is an expression, so it must produce a value. Two `Void`
        // branches agree on a type but leave nothing on the stack for the
        // surrounding expression to consume, which no backend can lower; it is
        // rejected here rather than reaching one.
        if ty == Type::Void {
            self.emit(
                span,
                "KSEM133",
                "a `? :` expression must produce a value, but both branches are `Void`".to_owned(),
            );
            return self.program.exprs.alloc(HirExpr::Error);
        }

        self.program.exprs.alloc(HirExpr::Select {
            cond: cond_hir,
            then: then_hir,
            otherwise: otherwise_hir,
            ty,
        })
    }
}
