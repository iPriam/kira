//! `try value as Type`: a failed downcast a handler can answer.
//!
//! A bare `value as Type` traps, because a cast that cannot answer has nothing
//! to hand back. Written under `try` inside an `attempt`, the same cast becomes
//! an ordinary fallible step: it yields a `Result`-shaped value carrying either
//! the payload or a `TypeCastError`, and the enclosing `handle` covers the
//! failure the way it covers every other one.
//!
//! # Why the failure point is written
//!
//! Kira has no unwinding, deliberately. `try` is accepted in exactly one
//! position — the whole initializer of a `let` inside an `attempt` — so every
//! place a body can leave early is visible in the body. A cast that routed its
//! failure to a handler on its own would be an invisible jump out of arbitrary
//! expression position, which is the thing that restriction exists to prevent.
//! So the cast says where its failure goes, and says it in the spelling every
//! other fallible step already uses.
//!
//! # What is minted
//!
//! Two enums, both the compiler's own and neither spellable in source: one
//! `TypeCastError` for the whole program, and one result row per target type.
//! They are minted rather than taken from Foundation because a cast is a
//! language operation: a program that imports nothing still writes one, and a
//! failure it cannot name is a failure it cannot handle.

use kira_semantics_model::hir::{HirExpr, HirExprId};
use kira_semantics_model::cast_result::{OWNING_MODULE, RESULT_TEMPLATE, TYPE_CAST_ERROR};
use kira_semantics_model::{EnumDef, EnumId, Instantiation, Type, VariantDef};
use kira_source::Span;
use kira_syntax_model::ast::{ExprId, TypeRefId};

use crate::analyze::{Analyzer, FnCtx};

impl Analyzer<'_> {
    /// Analyzes `value as Type` in the one position that can answer a failure:
    /// under a `try`.
    ///
    /// Returns the `Result`-shaped value the `attempt` machinery consumes, so
    /// this needs no rule of its own for exhaustiveness, agreement, or
    /// handler resolution: it produces what a fallible call produces.
    pub(crate) fn analyze_try_cast(
        &mut self,
        ctx: &mut FnCtx,
        value: ExprId,
        ty: TypeRefId,
        span: Span,
    ) -> HirExprId {
        let Some((operand, target)) = self.erased_operand_and_target(ctx, value, ty, span, "as")
        else {
            return self.program.exprs.alloc(HirExpr::Error);
        };
        let Some(failure) = self.type_cast_error_enum() else {
            return self.program.exprs.alloc(HirExpr::Error);
        };
        let Some(result) = self.cast_result_enum(target, failure) else {
            return self.program.exprs.alloc(HirExpr::Error);
        };
        self.program.exprs.alloc(HirExpr::TypeCastResult {
            value: operand,
            target,
            failure,
            ty: Type::Enum(result),
        })
    }

    /// The program's `TypeCastError`, minted on first use.
    ///
    /// One row for the whole program: the failure a cast reports is the same
    /// failure whatever it was casting to, and a handler written once covers
    /// every cast in its `attempt`.
    fn type_cast_error_enum(&mut self) -> Option<EnumId> {
        if let Some(known) = self.cast_error {
            return Some(known);
        }
        let id = self.program.types.enums_mut().declare(EnumDef {
            name: TYPE_CAST_ERROR.to_owned(),
            variants: vec![VariantDef {
                name: "Mismatch".to_owned(),
                // The descriptor of what the value actually held, which is the
                // one fact a handler cannot get any other way: the value it
                // asked about is gone by the time the failure exists.
                payload: Some(Type::RuntimeType),
            }],
        })?;
        self.program.types.enums_mut().set_module(id, OWNING_MODULE);
        self.cast_error = Some(id);
        Some(id)
    }

    /// The `Result`-shaped row a cast to `target` answers with, minted once per
    /// target.
    fn cast_result_enum(&mut self, target: Type, failure: EnumId) -> Option<EnumId> {
        if let Some(&known) = self.cast_results.get(&target) {
            return Some(known);
        }
        let name = format!("CastResult<{}>", self.type_name(target));
        let id = self.program.types.enums_mut().declare(EnumDef {
            name: name.clone(),
            variants: vec![
                VariantDef {
                    name: "Ok".to_owned(),
                    payload: Some(target),
                },
                VariantDef {
                    name: "Error".to_owned(),
                    payload: Some(Type::Enum(failure)),
                },
            ],
        })?;
        self.program.types.enums_mut().set_module(id, OWNING_MODULE);
        // Recorded as an instantiation so its runtime identity carries the
        // target the way every other generic row's does.
        self.program.types.enums_mut().record_instantiation(
            id,
            Instantiation {
                template: RESULT_TEMPLATE.to_owned(),
                arguments: vec![target],
            },
        );
        self.cast_results.insert(target, id);
        Some(id)
    }
}
