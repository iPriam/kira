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
        // `~` keeps the operand's spelling for the same reason `-` does: it is
        // one instruction on the raw bit pattern at every width and under
        // either signedness.
        (UnaryOp::BitNot, Type::Int(_)) => Some((HirUnaryOp::BitNot, operand)),
        _ => None,
    }
}

/// The type two numeric operands agree on, or `None` when they do not.
///
/// **The left operand decides.** When the two are compatible, the result is
/// `lt` — never `rt`, even when `rt` carries the written width and `lt` is a
/// bare `Int`/`Float`. So `plainInt / u8Value` is a *signed* divide producing a
/// plain `Int`, while `u8Value / plainInt` is an *unsigned* one producing a
/// `U8`. The two spellings are not interchangeable across the operator, and
/// that asymmetry is the language's rule, not an accident of this function:
/// arithmetic takes its result type from the left operand, and `/`, `%`, and
/// the four orderings take their signedness from the same place.
///
/// Compatibility is separate from, and looser than, that choice: a bare
/// `Int`/`Float` is a wildcard that pairs with any width, which is what makes
/// an integer literal usable at every width without a conversion rule. Two
/// *different* written widths agree on nothing — `u8Value + i64Value` is a type
/// error, not a promotion — because the language has no widening.
fn unify_numeric(lt: Type, rt: Type) -> Option<Type> {
    match (lt, rt) {
        (Type::Int(a), Type::Int(b)) if a == b => Some(lt),
        // A bare literal adapts to the written spelling on the other side,
        // whichever side that is: `1 + u8Value` is `U8` arithmetic exactly as
        // `u8Value + 1` is.
        (Type::Int(a), Type::Int(_)) if a == IntSpelling::Plain => Some(rt),
        (Type::Int(_), Type::Int(b)) if b == IntSpelling::Plain => Some(lt),
        (Type::Float(a), Type::Float(b)) if a == b => Some(lt),
        (Type::Float(a), Type::Float(_)) if a == FloatSpelling::Plain => Some(rt),
        (Type::Float(_), Type::Float(b)) if b == FloatSpelling::Plain => Some(lt),
        _ => None,
    }
}

/// The type the two branches of a `? :` agree on, or `None` when they do not.
///
/// Agreement is the same relation arithmetic uses, for the same reason: a bare
/// integer or float literal is a wildcard that pairs with any written width,
/// while two *different* written widths agree on nothing. Everything
/// non-numeric must match exactly — there is no widening and no common
/// supertype, because the language has no subtyping.
///
/// When both are numeric the written spelling decides, from whichever branch
/// wrote one, as in [`unify_numeric`]: `wide ? u8Value : 0` and `wide ? 0 :
/// u8Value` both type as `U8`. The width is observable, because it picks the
/// shift: `(true ? u64AllOnes : 0) >> 60` is an unsigned shift printing
/// `15`, while `(false ? 0 : u64AllOnes) >> 60` is a signed one printing `-1`.
/// `a_conditional_takes_its_width_from_the_then_branch` pins both spellings.
pub(crate) fn unify_branches(then: Type, otherwise: Type) -> Option<Type> {
    if then == otherwise {
        return Some(then);
    }
    unify_numeric(then, otherwise)
}

/// The symbolic spelling of a unary operator, for diagnostics.
pub(crate) fn unary_spelling(op: UnaryOp) -> &'static str {
    match op {
        UnaryOp::Neg => "-",
        UnaryOp::Not => "!",
        UnaryOp::BitNot => "~",
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
        B::BitAnd | B::BitOr | B::BitXor => bitwise(op, lt, rt),
        B::Shl | B::Shr => shift(op, lt, rt),
        _ => None,
    }
}

/// `&`, `|`, and `^`: two integers agreeing on a spelling, result that type.
///
/// These unify exactly as `+` does rather than demanding identical spellings,
/// so a bare integer literal is usable as a mask at any width — `flags & 0x0f`
/// types the same way `flags + 1` does. Floats, strings, and `Bool` are
/// rejected: there is no bitwise `Bool` operator, because `&&`/`||` already
/// occupy that meaning and are short-circuiting where these are not.
fn bitwise(op: BinaryOp, lt: Type, rt: Type) -> Option<(HirBinaryOp, Type)> {
    use BinaryOp as B;
    use HirBinaryOp as H;
    let ty = unify_numeric(lt, rt)?;
    if !matches!(ty, Type::Int(_)) {
        return None;
    }
    let hir = match op {
        B::BitAnd => H::BitAnd,
        B::BitOr => H::BitOr,
        B::BitXor => H::BitXor,
        _ => return None,
    };
    Some((hir, ty))
}

/// `<<` and `>>`: the result takes the **left** operand's type, and the shift
/// amount may be any integer spelling.
///
/// A shift is the one binary operator whose two operands are not required to
/// agree, and deliberately so: the right side is a count, not a value of the
/// same kind, so `u8Flags << i64Places` is well typed where `u8Flags +
/// i64Places` is not. Signedness is read off the left operand alone, which is
/// what decides whether `>>` propagates the sign or fills with zeros.
///
/// The shift amount must lie in `0..width` of the left operand at run time;
/// every backend traps on a count outside it.
fn shift(op: BinaryOp, lt: Type, rt: Type) -> Option<(HirBinaryOp, Type)> {
    use BinaryOp as B;
    use HirBinaryOp as H;
    if !matches!(lt, Type::Int(_)) || !matches!(rt, Type::Int(_)) {
        return None;
    }
    let hir = match op {
        B::Shl => H::Shl,
        B::Shr if lt.is_unsigned_int() => H::ShrUInt,
        B::Shr => H::ShrInt,
        _ => return None,
    };
    Some((hir, lt))
}

fn arithmetic(op: BinaryOp, lt: Type, rt: Type) -> Option<(HirBinaryOp, Type)> {
    use BinaryOp as B;
    use HirBinaryOp as H;
    // String concatenation is the one non-numeric arithmetic case.
    if op == B::Add && lt == Type::String && rt == Type::String {
        return Some((H::ConcatStr, Type::String));
    }
    let ty = unify_numeric(lt, rt)?;
    // `/` and `%` are the two arithmetic operators whose opcode depends on
    // signedness. `+`, `-`, and `*` share an opcode; the result type carries
    // the width, and every backend traps when the result leaves it: a `U8`
    // sum of 250 and 10 is an overflow, not 260 and not 4.
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
        (B::Rem, Type::Float(_)) => H::RemFloat,
        _ => return None,
    };
    Some((hir, ty))
}

fn comparison(op: BinaryOp, lt: Type, rt: Type) -> Option<(HirBinaryOp, Type)> {
    use BinaryOp as B;
    use HirBinaryOp as H;
    // As in `arithmetic`, `ty` is the left operand's type: an ordering compares
    // as signed or unsigned according to how the LHS was spelled.
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
        // Erasure already carries each value's kind at runtime so it can be
        // freed; reading that same kind back to compare costs no new
        // representation. This is why `Any == Any` holds where `Point ==
        // Point` does not — the concrete type has no runtime tag to consult.
        Type::Any if is_eq => H::EqAny,
        Type::Any => H::NeAny,
        // Two descriptors are equal exactly when they name one type by
        // package-qualified nominal identity, which is one word compared
        // against another: the ids are table rows, and one type has one row.
        Type::RuntimeType if is_eq => H::EqType,
        Type::RuntimeType => H::NeType,
        _ => return None,
    };
    Some((hir, Type::Bool))
}
