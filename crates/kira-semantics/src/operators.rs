//! Operator resolution: mapping a written operator and its operand types to a
//! typed HIR operator and result type.
//!
//! Split out of [`crate::typeck`] on the file-size ladder, and cohesive on its
//! own: these are the exhaustive tables that turn `+` on two `Int`s into
//! `AddInt`, so no backend re-derives operand types. [`equality_op`] is shared
//! with the `switch` desugar in [`crate::stmt`], which is why what a `case` may
//! match and what `==` accepts cannot drift — they are one function.

use kira_semantics_model::Type;
use kira_semantics_model::hir::{HirBinaryOp, HirUnaryOp};
use kira_syntax_model::ast::{BinaryOp, UnaryOp};

/// Resolves a unary operator against its operand type to a typed HIR op and
/// result type. Returns `None` for an unsupported combination.
pub(crate) fn resolve_unary(op: UnaryOp, operand: Type) -> Option<(HirUnaryOp, Type)> {
    match (op, operand) {
        (UnaryOp::Neg, Type::Int) => Some((HirUnaryOp::NegInt, Type::Int)),
        (UnaryOp::Neg, Type::Float) => Some((HirUnaryOp::NegFloat, Type::Float)),
        (UnaryOp::Not, Type::Bool) => Some((HirUnaryOp::Not, Type::Bool)),
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
    if lt != rt || !lt.is_numeric() {
        return None;
    }
    let hir = match (op, lt) {
        (B::Add, Type::Int) => H::AddInt,
        (B::Sub, Type::Int) => H::SubInt,
        (B::Mul, Type::Int) => H::MulInt,
        (B::Div, Type::Int) => H::DivInt,
        (B::Rem, Type::Int) => H::RemInt,
        (B::Add, Type::Float) => H::AddFloat,
        (B::Sub, Type::Float) => H::SubFloat,
        (B::Mul, Type::Float) => H::MulFloat,
        (B::Div, Type::Float) => H::DivFloat,
        _ => return None,
    };
    Some((hir, lt))
}

fn comparison(op: BinaryOp, lt: Type, rt: Type) -> Option<(HirBinaryOp, Type)> {
    use BinaryOp as B;
    use HirBinaryOp as H;
    if lt != rt || !lt.is_numeric() {
        return None;
    }
    let hir = match (op, lt) {
        (B::Lt, Type::Int) => H::LtInt,
        (B::Le, Type::Int) => H::LeInt,
        (B::Gt, Type::Int) => H::GtInt,
        (B::Ge, Type::Int) => H::GeInt,
        (B::Lt, Type::Float) => H::LtFloat,
        (B::Le, Type::Float) => H::LeFloat,
        (B::Gt, Type::Float) => H::GtFloat,
        (B::Ge, Type::Float) => H::GeFloat,
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
    if lt != rt {
        return None;
    }
    let is_eq = op == B::Eq;
    let hir = match lt {
        Type::Int if is_eq => H::EqInt,
        Type::Int => H::NeInt,
        Type::Float if is_eq => H::EqFloat,
        Type::Float => H::NeFloat,
        Type::Bool if is_eq => H::EqBool,
        Type::Bool => H::NeBool,
        Type::String if is_eq => H::EqStr,
        Type::String => H::NeStr,
        _ => return None,
    };
    Some((hir, Type::Bool))
}
