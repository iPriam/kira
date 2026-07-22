//! Scalar type-conversion calls: `Target(expr)` where `Target` is a numeric scalar type.
//!
//! The call form `Int(x)`, `U32(x)`, `Float(x)`, and the rest of the sized set
//! is a **value conversion**, not a function call. The scalar *types* already
//! resolve (see [`kira_semantics_model::ty`]); this recognizes the call form
//! and lowers it to a [`HirExpr::Convert`] the backends execute, so what was a
//! `KSEM061` "call to undefined function" becomes a real cast.
//!
//! The conversion matrix mirrors the language oracle: the operand must be an
//! `Int` or a `Float`, and the target must be an integer width (`Int`,
//! `I8`..`I64`, `U8`..`U64`) or a float width (`Float`, `F32`, `F64`). `Bool`
//! is neither a source nor a target, so `Bool(x)` is not a conversion and falls
//! through to the ordinary call path. Because every integer shares one 64-bit
//! representation and every float one 64-bit representation, an int-to-int and
//! a float-to-float conversion re-tag the type and copy the value unchanged;
//! only `Int`<->`Float` does runtime work. See [`ConvertKind`] for the exact
//! rules each kind carries.

use kira_semantics_model::Type;
use kira_semantics_model::hir::{ConvertKind, HirExpr, HirExprId};
use kira_source::Span;
use kira_syntax_model::ast::CallArg;

use crate::analyze::{Analyzer, FnCtx};

impl Analyzer<'_> {
    /// Recognizes and type-checks a scalar conversion call `Target(operand)`.
    ///
    /// Returns `None` when the call is not a numeric conversion at all — the
    /// callee does not name a numeric scalar type, or a local of that name
    /// shadows it — so the caller carries on to the ordinary call paths.
    /// Otherwise it owns the call and returns `Some`, reporting any mistake with
    /// a typed diagnostic rather than letting it reach the undefined-function
    /// path.
    pub(super) fn analyze_scalar_conversion(
        &mut self,
        ctx: &mut FnCtx,
        name: &str,
        args: &[CallArg],
        span: Span,
    ) -> Option<HirExprId> {
        let target = Type::from_name(name)?;
        // Only the numeric scalar types convert; `Bool`, `String`, and the rest
        // are not conversions and keep their ordinary meaning.
        if !target.is_numeric() {
            return None;
        }
        // A local of the same name shadows the type: `Int(x)` calls the local,
        // exactly as a local wins over a function of the same name elsewhere.
        if ctx.resolve(name).is_some() {
            return None;
        }
        // From here the call form is a numeric conversion, so this path owns it:
        // every branch returns `Some`, which is what keeps a mistake from also
        // being reported as an undefined function by the fallthrough.
        //
        // A conversion binds its operand by position, so a label on it is a
        // mistake.
        self.reject_argument_labels(args, "a numeric conversion");
        let values = Self::argument_values(args);
        if values.len() != 1 {
            // Analyze every argument so mistakes inside them still surface, then
            // report the arity against the conversion itself.
            for &value in &values {
                self.analyze_expr(ctx, value);
            }
            self.emit(
                span,
                "KSEM210",
                format!(
                    "a numeric conversion to `{name}` takes exactly one argument, found {}",
                    values.len()
                ),
            );
            return Some(self.program.exprs.alloc(HirExpr::Error));
        }

        let operand = self.analyze_expr(ctx, values[0]);
        let operand_ty = self.program.expr(operand).type_of();
        // An operand that already failed to analyze said so; do not pile a
        // conversion error on top of it.
        if operand_ty == Type::Error {
            return Some(self.program.exprs.alloc(HirExpr::Error));
        }
        let Some(kind) = conversion_kind(operand_ty, target) else {
            self.emit(
                span,
                "KSEM209",
                format!(
                    "`{}` cannot be converted to `{name}`: a numeric conversion takes an `Int` or \
                     a `Float`",
                    self.type_name(operand_ty)
                ),
            );
            return Some(self.program.exprs.alloc(HirExpr::Error));
        };
        Some(self.program.exprs.alloc(HirExpr::Convert {
            operand,
            kind,
            ty: target,
        }))
    }
}

/// The machine conversion from `from` to `to`, or `None` when `from` is not a
/// numeric type (`to` is already known numeric by its caller).
///
/// The four numeric pairs map to the four [`ConvertKind`] variants; a
/// non-numeric source is the one refused case.
fn conversion_kind(from: Type, to: Type) -> Option<ConvertKind> {
    Some(match (from, to) {
        (Type::Int(_), Type::Int(_)) => ConvertKind::IntToInt,
        (Type::Int(_), Type::Float(_)) => ConvertKind::IntToFloat,
        (Type::Float(_), Type::Int(_)) => ConvertKind::FloatToInt,
        (Type::Float(_), Type::Float(_)) => ConvertKind::FloatToFloat,
        _ => return None,
    })
}
