//! Scalar type-conversion calls: `Target(expr)` where `Target` is a numeric
//! scalar type or the opaque `RawPtr` word type.
//!
//! The call form `Int(x)`, `U32(x)`, `Float(x)`, and the rest of the sized set
//! is a **value conversion**, not a function call. The scalar *types* already
//! resolve (see [`kira_semantics_model::ty`]); this recognizes the call form
//! and lowers it to a [`HirExpr::Convert`] the backends execute, so what was a
//! `KSEM061` "call to undefined function" becomes a real cast.
//!
//! The conversion matrix mirrors the language oracle: numeric conversions take
//! an `Int` or a `Float`, and `RawPtr(expr)` takes an integer word. `Bool`
//! is neither a source nor a target, so `Bool(x)` is not a conversion and falls
//! through to the ordinary call path. Because every integer shares one 64-bit
//! representation and every float one 64-bit representation, an int-to-int,
//! float-to-float, or integer-to-`RawPtr` conversion only changes the VM tag;
//! only `Int`<->`Float` does arithmetic work. `rawPointerWord(pointer)` is the
//! inverse tag change from an opaque pointer word to `U64`.

use kira_semantics_model::hir::{ConvertKind, HirExpr, HirExprId};
use kira_semantics_model::{IntSpelling, Type};
use kira_source::Span;
use kira_syntax_model::ast::CallArg;

use crate::analyze::{Analyzer, FnCtx};

impl Analyzer<'_> {
    /// Recognizes and type-checks a scalar conversion call `Target(operand)`.
    ///
    /// Returns `None` when the call is not a scalar conversion at all — the
    /// callee does not name a numeric or `RawPtr` type, or a local of that name
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
        // Numeric scalar types and `RawPtr` convert; `Bool`, `String`, and the
        // rest keep their ordinary meaning.
        if !target.is_numeric() && target != Type::RawPtr {
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
            let message = if target == Type::RawPtr {
                format!(
                    "`{}` cannot be converted to `{name}`: `RawPtr` takes an integer word",
                    self.type_name(operand_ty)
                )
            } else {
                format!(
                    "`{}` cannot be converted to `{name}`: a numeric conversion takes an `Int` or \
                     a `Float`",
                    self.type_name(operand_ty)
                )
            };
            self.emit(span, "KSEM209", message);
            return Some(self.program.exprs.alloc(HirExpr::Error));
        };
        Some(self.program.exprs.alloc(HirExpr::Convert {
            operand,
            kind,
            ty: target,
        }))
    }

    /// Recognizes `rawPointerWord(pointer)`, the explicit conversion from an
    /// opaque pointer word to the integer representation used by a C callback
    /// ABI. Keeping this separate from numeric conversions makes the source
    /// and target types visible in the diagnostic and prevents an arbitrary
    /// integer from being mistaken for a valid pointer value.
    pub(super) fn analyze_raw_pointer_word(
        &mut self,
        ctx: &mut FnCtx,
        name: &str,
        args: &[CallArg],
        span: Span,
    ) -> Option<HirExprId> {
        if name != "rawPointerWord" || ctx.resolve(name).is_some() {
            return None;
        }
        let values = Self::argument_values(args);
        if values.len() != 1 {
            for &value in &values {
                self.analyze_expr(ctx, value);
            }
            self.emit(
                span,
                "KSEM210",
                format!(
                    "`rawPointerWord` takes exactly one argument, found {}",
                    values.len()
                ),
            );
            return Some(self.program.exprs.alloc(HirExpr::Error));
        }
        let operand = self.analyze_expr(ctx, values[0]);
        let operand_ty = self.program.expr(operand).type_of();
        if operand_ty == Type::Error {
            return Some(self.program.exprs.alloc(HirExpr::Error));
        }
        if !matches!(operand_ty, Type::RawPtr | Type::ForeignPtr(_)) {
            self.emit(
                span,
                "KSEM209",
                format!(
                    "`rawPointerWord` takes a `RawPtr`, found `{}`",
                    self.type_name(operand_ty)
                ),
            );
            return Some(self.program.exprs.alloc(HirExpr::Error));
        }
        Some(self.program.exprs.alloc(HirExpr::Convert {
            operand,
            kind: ConvertKind::RawPtrToInt,
            ty: Type::Int(IntSpelling::U64),
        }))
    }

    /// Recognizes `floatToBits(x)`, `bitsToFloat(x)`, and `bitsToFloat32(x)`,
    /// the IEEE-754
    /// **reinterpretations**.
    ///
    /// These are not conversions and are deliberately not spelled like one:
    /// `U64(x)` on a `Float` rounds and saturates, which is the right answer for
    /// a number and the wrong one for a bit pattern. Serializing a float byte
    /// for byte needs the bits exactly as they are, NaN payload included, so it
    /// gets a name of its own.
    ///
    /// Returns `None` when the call is not one of the two, or when a local of
    /// that name shadows it — the same rule a conversion follows.
    pub(super) fn analyze_bit_reinterpret(
        &mut self,
        ctx: &mut FnCtx,
        name: &str,
        args: &[CallArg],
        span: Span,
    ) -> Option<HirExprId> {
        let (kind, from, to) = match name {
            "floatToBits" => (
                ConvertKind::FloatToBits,
                Type::FLOAT,
                Type::Int(IntSpelling::U64),
            ),
            "bitsToFloat" => (
                ConvertKind::BitsToFloat,
                Type::Int(IntSpelling::U64),
                Type::FLOAT,
            ),
            // Binary data is full of 32-bit floats — a mesh, a texture, a
            // packed vertex — and the same bits mean a different number at the
            // two widths, so reading one needs its own reinterpretation.
            "bitsToFloat32" => (
                ConvertKind::Bits32ToFloat,
                Type::Int(IntSpelling::U32),
                Type::FLOAT,
            ),
            // Writing that same binary data back out — a cooked mesh, a vertex
            // buffer — needs the other direction, and it is not a round trip
            // through `floatToBits`: the value narrows to 32 bits first.
            "floatToBits32" => (
                ConvertKind::FloatToBits32,
                Type::FLOAT,
                Type::Int(IntSpelling::U32),
            ),
            _ => return None,
        };
        if ctx.resolve(name).is_some() {
            return None;
        }
        let values = Self::argument_values(args);
        if values.len() != 1 {
            for &value in &values {
                self.analyze_expr(ctx, value);
            }
            self.emit(
                span,
                "KSEM210",
                format!(
                    "`{name}` takes exactly one argument, found {}",
                    values.len()
                ),
            );
            return Some(self.program.exprs.alloc(HirExpr::Error));
        }

        let operand = self.analyze_expr(ctx, values[0]);
        let operand_ty = self.program.expr(operand).type_of();
        if operand_ty == Type::Error {
            return Some(self.program.exprs.alloc(HirExpr::Error));
        }
        if !operand_ty.assignable_to(from) {
            self.emit(
                span,
                "KSEM209",
                format!(
                    "`{name}` takes a `{}`, found `{}`",
                    self.type_name(from),
                    self.type_name(operand_ty)
                ),
            );
            return Some(self.program.exprs.alloc(HirExpr::Error));
        }
        Some(self.program.exprs.alloc(HirExpr::Convert {
            operand,
            kind,
            ty: to,
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
        (Type::Int(_), Type::RawPtr) => ConvertKind::IntToRawPtr,
        _ => return None,
    })
}
