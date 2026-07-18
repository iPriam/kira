//! Operator resolution: mapping a written operator and its operand types to a
//! typed HIR operator and result type.
//!
//! Split out of [`crate::typeck`] on the file-size ladder, and cohesive on its
//! own: these are the exhaustive tables that turn `+` on two `Int`s into
//! `AddInt`, so no backend re-derives operand types. [`equality_op`] is shared
//! with the `switch` desugar in [`crate::stmt`], which is why what a `case` may
//! match and what `==` accepts cannot drift — they are one function.

use kira_semantics_model::hir::{HirBinaryOp, HirUnaryOp};
use kira_semantics_model::{FloatSpelling, IntSpelling, Type};
use kira_syntax_model::ast::{BinaryOp, UnaryOp};

/// Resolves a unary operator against its operand type to a typed HIR op and
/// result type. Returns `None` for an unsupported combination.
pub(crate) fn resolve_unary(op: UnaryOp, operand: Type) -> Option<(HirUnaryOp, Type)> {
    match (op, operand) {
        // Negation keeps the operand's spelling: `-x` on an `I32` is an `I32`.
        // It is the same instruction at every width, and on an unsigned one
        // too — negation is two's-complement wrapping, which is
        // signedness-free.
        (UnaryOp::Neg, Type::Int(_)) => Some((HirUnaryOp::NegInt, operand)),
        (UnaryOp::Neg, Type::Float(_)) => Some((HirUnaryOp::NegFloat, operand)),
        (UnaryOp::Not, Type::Bool) => Some((HirUnaryOp::Not, Type::Bool)),
        _ => None,
    }
}

/// The single type two numeric operands agree on, or `None` when they do not.
///
/// A bare `Int`/`Float` is a wildcard, so it yields to a written width: `x + 1`
/// on a `U8` local is a `U8`, which is what makes an integer literal usable at
/// every width without a conversion rule. Two *different* written widths agree
/// on nothing — `u8Value + i64Value` is a type error, not a promotion — because
/// the language has no widening.
fn unify_numeric(lt: Type, rt: Type) -> Option<Type> {
    match (lt, rt) {
        (Type::Int(a), Type::Int(b)) => match (a, b) {
            _ if a == b => Some(lt),
            (IntSpelling::Plain, _) => Some(rt),
            (_, IntSpelling::Plain) => Some(lt),
            _ => None,
        },
        (Type::Float(a), Type::Float(b)) => match (a, b) {
            _ if a == b => Some(lt),
            (FloatSpelling::Plain, _) => Some(rt),
            (_, FloatSpelling::Plain) => Some(lt),
            _ => None,
        },
        _ => None,
    }
}

/// The symbolic spelling of a unary operator, for diagnostics.
pub(crate) fn unary_spelling(op: UnaryOp) -> &'static str {
    match op {
        UnaryOp::Neg => "-",
        UnaryOp::Not => "!",
    }
}

/// Resolves a binary operator against its operand types to a typed HIR op and
/// result type. Returns `None` for an unsupported combination.
pub(crate) fn resolve_binary(op: BinaryOp, lt: Type, rt: Type) -> Option<(HirBinaryOp, Type)> {
    use BinaryOp as B;
    use HirBinaryOp as H;
    match op {
        B::Add | B::Sub | B::Mul | B::Div | B::Rem => arithmetic(op, lt, rt),
        B::Lt | B::Le | B::Gt | B::Ge => comparison(op, lt, rt),
        B::Eq | B::Ne => equality(op, lt, rt),
        B::And if lt == Type::Bool && rt == Type::Bool => Some((H::And, Type::Bool)),
        B::Or if lt == Type::Bool && rt == Type::Bool => Some((H::Or, Type::Bool)),
        _ => None,
    }
}

fn arithmetic(op: BinaryOp, lt: Type, rt: Type) -> Option<(HirBinaryOp, Type)> {
    use BinaryOp as B;
    use HirBinaryOp as H;
    // String concatenation is the one non-numeric arithmetic case.
    if op == B::Add && lt == Type::String && rt == Type::String {
        return Some((H::ConcatStr, Type::String));
    }
    let ty = unify_numeric(lt, rt)?;
    // `/` and `%` are the two arithmetic operators whose result depends on
    // signedness. `+`, `-`, and `*` wrap identically either way, and at 64 bits
    // for every width: a `U8` sum of 250 and 10 is 260, not 4. Narrowing to the
    // written width is behavior the language does not define, so nothing here
    // masks.
    let unsigned = ty.is_unsigned_int();
    let hir = match (op, ty) {
        (B::Add, Type::Int(_)) => H::AddInt,
        (B::Sub, Type::Int(_)) => H::SubInt,
        (B::Mul, Type::Int(_)) => H::MulInt,
        (B::Div, Type::Int(_)) if unsigned => H::DivUInt,
        (B::Div, Type::Int(_)) => H::DivInt,
        (B::Rem, Type::Int(_)) if unsigned => H::RemUInt,
        (B::Rem, Type::Int(_)) => H::RemInt,
        (B::Add, Type::Float(_)) => H::AddFloat,
        (B::Sub, Type::Float(_)) => H::SubFloat,
        (B::Mul, Type::Float(_)) => H::MulFloat,
        (B::Div, Type::Float(_)) => H::DivFloat,
        _ => return None,
    };
    Some((hir, ty))
}

fn comparison(op: BinaryOp, lt: Type, rt: Type) -> Option<(HirBinaryOp, Type)> {
    use BinaryOp as B;
    use HirBinaryOp as H;
    let ty = unify_numeric(lt, rt)?;
    let unsigned = ty.is_unsigned_int();
    let hir = match (op, ty) {
        (B::Lt, Type::Int(_)) if unsigned => H::LtUInt,
        (B::Le, Type::Int(_)) if unsigned => H::LeUInt,
        (B::Gt, Type::Int(_)) if unsigned => H::GtUInt,
        (B::Ge, Type::Int(_)) if unsigned => H::GeUInt,
        (B::Lt, Type::Int(_)) => H::LtInt,
        (B::Le, Type::Int(_)) => H::LeInt,
        (B::Gt, Type::Int(_)) => H::GtInt,
        (B::Ge, Type::Int(_)) => H::GeInt,
        (B::Lt, Type::Float(_)) => H::LtFloat,
        (B::Le, Type::Float(_)) => H::LeFloat,
        (B::Gt, Type::Float(_)) => H::GtFloat,
        (B::Ge, Type::Float(_)) => H::GeFloat,
        _ => return None,
    };
    Some((hir, Type::Bool))
}

/// The `==` operator for comparing `subject` against `label`, or `None` when
/// the two cannot be compared.
///
/// A `switch` arm is `subject == label`, so what a `case` may match is decided
/// here rather than by a second rule that could drift from this one.
pub(crate) fn equality_op(subject: Type, label: Type) -> Option<HirBinaryOp> {
    equality(BinaryOp::Eq, subject, label).map(|(op, _)| op)
}

fn equality(op: BinaryOp, lt: Type, rt: Type) -> Option<(HirBinaryOp, Type)> {
    use BinaryOp as B;
    use HirBinaryOp as H;
    // Numerics unify on their spelling; everything else must match exactly.
    // Equality alone among the comparisons has no unsigned twin: it compares
    // bit patterns, which are the same under either signedness.
    let lt = match (lt.is_numeric(), rt.is_numeric()) {
        (true, true) => unify_numeric(lt, rt)?,
        _ if lt == rt => lt,
        _ => return None,
    };
    let is_eq = op == B::Eq;
    let hir = match lt {
        Type::Int(_) if is_eq => H::EqInt,
        Type::Int(_) => H::NeInt,
        Type::Float(_) if is_eq => H::EqFloat,
        Type::Float(_) => H::NeFloat,
        Type::Bool if is_eq => H::EqBool,
        Type::Bool => H::NeBool,
        Type::String if is_eq => H::EqStr,
        Type::String => H::NeStr,
        _ => return None,
    };
    Some((hir, Type::Bool))
}
