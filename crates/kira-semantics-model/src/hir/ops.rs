//! Call targets and resolved operators, split out of [`super`] on the
//! file-size ladder.
//!
//! One module because these four enums answer one question between them — what
//! a [`super::HirExpr::Call`], [`super::HirExpr::Unary`], or
//! [`super::HirExpr::Binary`] node *does* — and because none of them mentions
//! the tree they sit in. They are the vocabulary the expression arena refers
//! to, resolved once during analysis so nothing below re-derives an operator
//! from operand types.

use super::{ForeignId, FuncId};

/// The target of a call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Callee {
    /// A language builtin.
    Builtin(Builtin),
    /// A user-defined function.
    User(FuncId),
    /// A foreign C function, indexed into [`HirProgram::foreign`].
    ///
    /// The call site is ordinary Kira — no `@Native`, no ceremony — and the
    /// registry row carries the exact-width signature the call was checked
    /// against.
    Foreign(ForeignId),
}

/// The builtins the v0 subset provides.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Builtin {
    /// `print(value)` — writes one formatted line of output.
    Print,
    /// `taskYield()` — a cooperative suspend point.
    ///
    /// The executor hands the next runnable task a turn and comes back here;
    /// with nothing else queued it is a no-op, which is what makes calling it
    /// outside a task body legal rather than a special case.
    TaskYield,
    /// `taskSleep(ms)` — park, moving the virtual clock forward by `ms`.
    TaskSleep,
}

/// A type-resolved unary operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HirUnaryOp {
    /// Integer negation.
    NegInt,
    /// Float negation.
    NegFloat,
    /// Boolean negation.
    Not,
    /// Bitwise complement (`~`) on the raw 64-bit pattern.
    BitNot,
}

/// A type-resolved binary operator: each variant fixes its operand types, so
/// backends never re-derive types from operands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HirBinaryOp {
    /// Integer `+`, `-`, `*`, `/`, `%`.
    AddInt,
    /// Integer subtraction.
    SubInt,
    /// Integer multiplication.
    MulInt,
    /// `wrappingAdd(a, b)`: integer addition that wraps at the operands'
    /// width instead of trapping.
    WrappingAddInt,
    /// `wrappingSub(a, b)`, wrapping at the operands' width.
    WrappingSubInt,
    /// `wrappingMul(a, b)`, wrapping at the operands' width.
    WrappingMulInt,
    /// Integer division (truncating), signed.
    DivInt,
    /// Integer remainder, signed.
    RemInt,
    /// Integer division (truncating), unsigned — the `U8`..`U64` spellings.
    ///
    /// Separate from [`HirBinaryOp::DivInt`] because signedness is the one
    /// thing an integer's written width decides. `+`, `-`, and `*` need no
    /// unsigned twin: two's-complement wrapping is bit-identical for both
    /// signednesses, so they would be the same instruction.
    DivUInt,
    /// Integer remainder, unsigned — the `U8`..`U64` spellings.
    RemUInt,
    /// Float addition.
    AddFloat,
    /// Float subtraction.
    SubFloat,
    /// Float multiplication.
    MulFloat,
    /// Float division.
    DivFloat,
    /// Float remainder, truncated: the sign follows the dividend, so
    /// `-9.0 % 4.0` is `-1.0` rather than the `3.0` a floored remainder gives.
    RemFloat,
    /// String concatenation (`+`).
    ConcatStr,
    /// Integer comparisons.
    EqInt,
    /// Integer inequality.
    NeInt,
    /// Integer less-than.
    LtInt,
    /// Integer less-or-equal.
    LeInt,
    /// Integer greater-than.
    GtInt,
    /// Integer greater-or-equal.
    GeInt,
    /// Integer less-than, unsigned — the `U8`..`U64` spellings.
    ///
    /// Ordering needs an unsigned twin for the same reason division does, and
    /// equality does not: `==` compares bit patterns, which is signedness-free.
    LtUInt,
    /// Integer less-or-equal, unsigned.
    LeUInt,
    /// Integer greater-than, unsigned.
    GtUInt,
    /// Integer greater-or-equal, unsigned.
    GeUInt,
    /// Float comparisons.
    EqFloat,
    /// Float inequality.
    NeFloat,
    /// Float less-than.
    LtFloat,
    /// Float less-or-equal.
    LeFloat,
    /// Float greater-than.
    GtFloat,
    /// Float greater-or-equal.
    GeFloat,
    /// Boolean equality.
    EqBool,
    /// Boolean inequality.
    NeBool,
    /// String equality.
    EqStr,
    /// String inequality.
    NeStr,
    /// Structural equality of two erased values (`Any`).
    ///
    /// The one comparison whose operand types are unknown until it runs. Both
    /// sides carry their own kind — the VM in its value tag, native code in the
    /// erasure box's [`kira_runtime_abi::ErasedKind`] — so the comparison reads
    /// those first and answers `false` for a mismatch rather than trapping.
    /// Two values of the same kind then compare by structure: scalars by bit
    /// pattern, strings by bytes, and aggregates field-by-field and
    /// element-by-element.
    ///
    /// Deliberately wider than `==` on the concrete types: a `Point` may not be
    /// compared to a `Point`, because that would commit the language to
    /// structural equality for every struct. Erasure is the one place a value's
    /// structure is already carried at runtime for the sake of freeing it, so
    /// comparing it costs no new representation.
    EqAny,
    /// Structural inequality of two erased values (`Any`).
    NeAny,
    /// Short-circuiting logical AND.
    And,
    /// Short-circuiting logical OR.
    Or,
    /// Bitwise AND (`&`) on the raw 64-bit pattern.
    ///
    /// The three bitwise operators need no unsigned twin for the same reason
    /// `+` does not: they act on bits, and a bit has no sign.
    BitAnd,
    /// Bitwise OR (`|`) on the raw 64-bit pattern.
    BitOr,
    /// Bitwise XOR (`^`) on the raw 64-bit pattern.
    BitXor,
    /// Left shift (`<<`). The shift amount is taken modulo 64.
    ///
    /// Signedness-free: shifting bits left discards the high end either way.
    Shl,
    /// Arithmetic right shift (`>>`), sign-propagating — the signed spellings.
    ///
    /// Unlike `<<`, `>>` *does* need an unsigned twin: what fills the vacated
    /// high bits is exactly the question signedness answers.
    ShrInt,
    /// Logical right shift (`>>`), zero-filling — the `U8`..`U64` spellings.
    ShrUInt,
}
