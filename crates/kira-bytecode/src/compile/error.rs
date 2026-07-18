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
