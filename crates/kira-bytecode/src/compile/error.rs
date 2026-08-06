//! The typed failures the bytecode compiler reports.
//!
//! Split out of [`crate::compile`] so the compiler body stays one readable
//! walk: these are the vocabulary, not the algorithm. Every variant is either a
//! real limit of the bytecode format (a count that outgrew its operand width)
//! or an invariant the frontend was supposed to have enforced — and each says
//! which, because the two mean very different things to whoever reads one.

/// An error raised while lowering IR to bytecode.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CompileError {
    /// The mid stage could not decide what a function releases.
    ///
    /// A contradiction inside one function rather than anything the source
    /// said, so it surfaces as a compiler fault. Reported rather than defaulted
    /// to "release nothing": a function compiled with an empty plan links and
    /// runs and leaks every value it holds.
    #[error("cannot plan releases: {0}")]
    ReleasePlan(#[from] kira_ir::mid::MidError),
    /// A function needs more local slots than the format's `u16` can address.
    #[error("function `{function}` needs {count} local slots; the bytecode format allows 65535")]
    TooManyLocals {
        /// The offending function's name.
        function: String,
        /// The requested number of local slots.
        count: u32,
    },
    /// An IR expression referenced a local slot beyond the `u16` range.
    #[error("function `{function}` references local slot {slot}, beyond the format's 65535")]
    LocalSlotOutOfRange {
        /// The offending function's name.
        function: String,
        /// The out-of-range slot index.
        slot: u32,
    },
    /// The program has more distinct string constants than the pool can index.
    #[error("program has too many distinct string constants for the bytecode format")]
    TooManyStrings,
    /// Internal invariant: a jump patch landed on a non-jump instruction.
    #[error("bytecode compiler invariant violated: patch target is not a jump")]
    PatchedNonJump,
    /// Internal invariant: a short-circuit operator reached opcode selection.
    #[error("bytecode compiler invariant violated: short-circuit operator has no opcode")]
    ShortCircuitOpcode,
    /// Internal invariant: a type with no runtime value reached an erasure.
    ///
    /// Nothing analysis admits can get here — `Void`, `Cell`, `Task`, and
    /// `NativeState` are all refused by `Type::assignable_to` before `Any`
    /// takes them — so this is a compiler bug surfaced typed rather than a
    /// program the user can write.
    #[error("bytecode compiler invariant violated: a type with no value was erased into `Any`")]
    ErasureOfAValuelessType,
    /// A program has more functions than the format's `u32` call operand.
    #[error("the program has {count} functions; the format allows 4294967295")]
    TooManyFunctions {
        /// How many functions the program has.
        count: usize,
    },
    /// Internal invariant: a widening reached codegen with a non-enum row.
    ///
    /// Only a generic instantiation widens, and `TypeTable::admits` refuses
    /// every other pair before lowering runs.
    #[error("bytecode compiler invariant violated: a widening of something not an enum")]
    WidenedNonEnum,
    /// Internal invariant: a widening named an enum the program never declared.
    #[error("bytecode compiler invariant violated: a widening of an undeclared enum")]
    WidenedUndeclaredEnum,
    /// Internal invariant: the two rows of a widening disagree about their
    /// variants — a different count, or one carrying a payload where the other
    /// does not.
    #[error("bytecode compiler invariant violated: a widening between rows that disagree")]
    WidenedMismatchedRows,
    /// Internal invariant: a widened payload crossed to a type the type rule
    /// admits no crossing to.
    #[error("bytecode compiler invariant violated: a widening of a payload the type rule refuses")]
    WidenedPayloadTypeRefused,
    /// Internal invariant: a `break`/`continue` reached codegen with no
    /// enclosing loop, which analysis is supposed to have rejected.
    #[error(
        "bytecode compiler invariant violated: `break`/`continue` outside a loop in `{function}`"
    )]
    JumpOutsideLoop {
        /// The offending function's name.
        function: String,
    },
    /// A struct has more fields than the format's `u16` operand can count.
    #[error("function `{function}` builds a struct of {count} fields; the format allows 65535")]
    TooManyFields {
        /// The offending function's name.
        function: String,
        /// The requested number of fields.
        count: usize,
    },
    /// A nested field assignment walks deeper than the format can encode.
    #[error("function `{function}` assigns through {count} nested fields; the format allows 65535")]
    FieldPathTooDeep {
        /// The offending function's name.
        function: String,
        /// The requested path depth.
        count: usize,
    },
    /// An array literal has more elements than the format's `u32` can count.
    #[error("function `{function}` builds an array of {count} elements; the format allows 2^32-1")]
    TooManyElements {
        /// The offending function's name.
        function: String,
        /// The requested number of elements.
        count: usize,
    },
    /// An enum has a variant tag beyond the format's `u16` operand.
    #[error(
        "function `{function}` constructs enum variant #{tag}; the format allows 65535 variants"
    )]
    TooManyVariants {
        /// The offending function's name.
        function: String,
        /// The out-of-range variant tag.
        tag: u32,
    },
    /// Internal invariant: a place with an array index reached the static
    /// field-path encoder, which cannot express one.
    #[error(
        "bytecode compiler invariant violated: dynamic index in a static field path in `{function}`"
    )]
    DynamicFieldPath {
        /// The offending function's name.
        function: String,
    },
    /// Internal invariant: a call to a mutating method carried no writeback
    /// place, or one carried a writeback but its callee is not a user function —
    /// the frontend records a writeback for every mutating-method call and only
    /// a user method can be one, so either is a broken lowering.
    #[error(
        "bytecode compiler invariant violated: mutating-method call in `{function}` is malformed \
         (missing writeback or non-user callee)"
    )]
    MalformedMutCall {
        /// The offending function's name.
        function: String,
    },
    /// A mutating method was assigned to the native half. A struct-receiver
    /// method cannot cross the seam, so a mutating call is always same-engine;
    /// reaching this means the split placed one across it.
    #[error(
        "function `{function}` calls `{callee}`, a mutating method on the native engine, which \
         structs cannot cross"
    )]
    MutCallAcrossSeam {
        /// The offending function's name.
        function: String,
        /// The mutating method that was placed across the seam.
        ///
        /// Named because the caller is often a closure with a synthesized name,
        /// and the fix is always at the callee: it is the one carrying the
        /// annotation that put a struct receiver on the far side.
        callee: String,
    },
    /// Internal invariant: an export's signature names a type that cannot cross
    /// the export boundary, which the frontend refuses before this runs.
    #[error(
        "bytecode compiler invariant violated: export `{export}` names `{ty}`, which cannot cross \
         the export boundary"
    )]
    UncrossableExport {
        /// The offending export's consumer-facing name.
        export: String,
        /// The type that cannot cross, as the author would recognize it.
        ty: String,
    },
}
