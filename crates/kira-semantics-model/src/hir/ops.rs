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
    /// `abort(message)` — writes the message and hard-traps (no unwind).
    ///
    /// The unrecoverable-failure primitive: it does not return, so it is how a
    /// test assertion or an invariant check ends a run the child-runner then
    /// records as a trap. It is a hard trap, not a recoverable `panic!` — there
    /// is no unwinding, matching how the language already aborts on an
    /// out-of-bounds index or an overflow.
    Abort,
    /// `taskYield()` — a cooperative suspend point.
    ///
    /// The executor hands the next runnable task a turn and comes back here;
    /// with nothing else queued it is a no-op, which is what makes calling it
    /// outside a task body legal rather than a special case.
    TaskYield,
    /// `taskSleep(ms)` — park, moving the virtual clock forward by `ms`.
    TaskSleep,
    /// `fromCode<E>(code)` — the payload-less enum variant whose declaration
    /// index is `code`, falling back to the first variant for a code outside
    /// `0..variantCount`.
    ///
    /// The inverse of the tag read behind [`crate::hir::HirExpr::EnumTag`], and
    /// the native form of what `@Derive(Tagged)` generated as `<Enum>_fromCode`.
    /// The call carries two `Int` arguments — the code, then the enum's variant
    /// count (baked in by the analyzer from the type argument) — so the backend
    /// clamps and builds the inline enum without a type lookup; its result type
    /// is the named enum.
    FromCode,
    /// `hash(value)` — fold a `Hashable` value into one `Int`, structurally and
    /// consistently with `==`.
    ///
    /// The native operation behind the `Hashable` trait, the fold twin of the
    /// `EqValue` walk: a struct folds its fields, an array its length then
    /// elements, an enum its tag then payload, each leaf an integer, a boolean,
    /// a string's bytes, or a decimal's value. Floats are refused by the
    /// classifier, so two values that are `==` always hash the same.
    Hash,
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
    /// The erased twin of [`EqValue`]: it reads each side's kind at run time
    /// rather than being told the type at compile time. Two values of the same
    /// kind then compare by the same structural rule — scalars by bit pattern,
    /// strings by bytes, aggregates field-by-field and element-by-element — so
    /// `Any` equality and concrete-type equality never disagree.
    EqAny,
    /// Structural inequality of two erased values (`Any`).
    NeAny,
    /// Structural equality of two values of one statically known type
    /// (`EqValue`), the operator behind `==` on a struct, an array, or a
    /// payload-carrying enum.
    ///
    /// The type checker has already settled that both operands are the same
    /// type and that the type conforms to `Equatable` — every leaf is itself
    /// comparable — so the comparison walks the shape without a runtime kind
    /// tag: a struct field-by-field, an array by length then element-by-element,
    /// an enum by tag then payload, each leaf bottoming out at a scalar, string,
    /// or another conforming aggregate. This is the same walk [`EqAny`] runs
    /// once erasure has recovered the type; the two share one implementation on
    /// every backend so they can never drift.
    ///
    /// A type carrying a non-comparable leaf — an opaque handle, a callback,
    /// bare native state — never reaches here: it fails `Equatable` conformance
    /// at the comparison site with a diagnostic naming the leaf, rather than
    /// falling back to identity.
    EqValue,
    /// Structural inequality of two values of one statically known type.
    NeValue,
    /// Structural three-way comparison of two values of one statically known
    /// type, the operator behind `<`, `<=`, `>`, `>=` on a struct, an array, or
    /// a payload-carrying enum. Answers a plain `Int`: negative when the left is
    /// ordered before the right, zero when they are equal, positive otherwise.
    ///
    /// The type checker has already settled that both operands are the same type
    /// and that the type conforms to `Ordered` — every leaf is itself totally
    /// ordered — so the comparison walks the shape without a runtime kind tag,
    /// lexicographically: a struct field-by-field in declaration order, an array
    /// element-by-element then by length, an enum by tag then payload, each leaf
    /// bottoming out at a scalar, a string's bytes, or another ordered
    /// aggregate. The first pair that differs decides; equal pairs walk on. It
    /// shares the structural nesting [`EqValue`] walks so the two can never
    /// disagree about a type's shape, and each backend wraps the ordinary
    /// integer comparison of its result against zero to answer the four
    /// orderings from one walk.
    ///
    /// A type carrying a leaf with no total order — a pointer word, an opaque
    /// handle, bare native state, `Any` — never reaches here: it fails `Ordered`
    /// conformance at the comparison site with a diagnostic naming the leaf.
    CmpValue,
    /// Identity equality of two runtime type descriptors (`Type`).
    ///
    /// One word against another: a type has one descriptor row, so equal ids
    /// mean one package-qualified nominal identity and unequal ids mean two.
    EqType,
    /// Identity inequality of two runtime type descriptors (`Type`).
    NeType,
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
