//! `value is Type` and `value as Type`: asking an erased value what it holds.
//!
//! Both read an `Any`. `is` answers with a `Bool` and releases the value;
//! `as` hands back the payload as an owned value of the named type, and a
//! payload of any other type is a trap on every engine. The type named must
//! be one a value erases *into* `Any` as — there is nothing an `Any` could
//! hold that a `Void` or another `Any` would name.

use kira_semantics_model::ErasedTypeId;
use kira_semantics_model::hir::{HirExpr, HirExprId};
use kira_semantics_model::Type;
use kira_source::Span;
use kira_syntax_model::ast::{ExprId, TypeRefId};

use crate::analyze::{Analyzer, FnCtx};

impl Analyzer<'_> {
    /// `value is Type`.
    pub(super) fn analyze_type_test(
        &mut self,
        ctx: &mut FnCtx,
        value: ExprId,
        ty: TypeRefId,
        span: Span,
    ) -> HirExprId {
        let Some((value, target)) = self.erased_operand_and_target(ctx, value, ty, span, "is")
        else {
            return self.program.exprs.alloc(HirExpr::Error);
        };
        self.program.exprs.alloc(HirExpr::TypeTest { value, target })
    }

    /// `value as Type`.
    pub(super) fn analyze_type_cast(
        &mut self,
        ctx: &mut FnCtx,
        value: ExprId,
        ty: TypeRefId,
        span: Span,
    ) -> HirExprId {
        let Some((value, target)) = self.erased_operand_and_target(ctx, value, ty, span, "as")
        else {
            return self.program.exprs.alloc(HirExpr::Error);
        };
        self.program.exprs.alloc(HirExpr::TypeCast { value, target })
    }

    /// The analyzed `Any` operand and the resolved target of `is` or `as`, or
    /// `None` once the mistake is reported.
    fn erased_operand_and_target(
        &mut self,
        ctx: &mut FnCtx,
        value: ExprId,
        ty: TypeRefId,
        span: Span,
        operator: &str,
    ) -> Option<(HirExprId, Type)> {
        let value = self.analyze_expr_expecting(ctx, value, Some(Type::Any));
        let actual = self.program.expr(value).type_of();
        let target = self.resolve_type_ref(ty);
        if actual == Type::Error || target == Type::Error {
            return None;
        }
        if actual != Type::Any {
            self.emit(
                span,
                "KSEM358",
                format!(
                    "`{operator}` asks an `Any` what it holds, and this value is a `{}`, whose \
                     type is already known",
                    self.type_name(actual)
                ),
            );
            return None;
        }
        if ErasedTypeId::of(target).is_none() {
            self.emit(
                span,
                "KSEM359",
                format!(
                    "`{}` is not a type an `Any` can hold, so `{operator} {0}` can never be true",
                    self.type_name(target)
                ),
            );
            return None;
        }
        Some((value, target))
    }
}
