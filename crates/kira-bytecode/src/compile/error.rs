//! The typed failures the bytecode compiler reports.
//!
//! Split out of [`crate::compile`] so the compiler body stays one readable
//! walk: these are the vocabulary, not the algorithm. Each variant is an
//! invariant or an input mismatch that remains meaningful after lowering.

/// An error raised while lowering IR to bytecode.
/// `Eq` is not derived: [`CompileError::Malformed`] carries a validation fault,
/// which carries a float bound in one of its variants.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum CompileError {
    /// The mid stage could not decide what a function releases.
    ///
    /// A contradiction inside one function rather than anything the source
    /// said, so it surfaces as a compiler fault. Reported rather than defaulted
    /// to "release nothing": a function compiled with an empty plan links and
    /// runs and leaks every value it holds.
    #[error("cannot plan releases: {0}")]
    ReleasePlan(#[from] kira_ir::mid::MidError),
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
    /// A read through an `@FFI.Pointer` names a member the target's C layout
    /// does not describe.
    #[error("function `{function}` reads member {member} of a C layout that has no such member")]
    ForeignMemberMissing {
        /// The function being compiled.
        function: String,
        /// The member index the read asked for.
        member: u32,
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
    /// Internal invariant: a main-thread request named no lowered function.
    #[error("main-thread request in `{function}` names function {target}, which is not in the IR")]
    UnknownMainThreadTarget {
        /// The function containing the request.
        function: String,
        /// The requested target index.
        target: u32,
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
    /// An exported class index cannot cross the fixed-width export seam.
    #[error("export `{export}` needs class index {class}, beyond the export boundary's u32 index")]
    ExportClassIndexTooLarge {
        /// The export whose signature introduced the class.
        export: String,
        /// The zero-based class index that could not be represented.
        class: usize,
    },
    /// The module this compile produced does not satisfy the format's own rules.
    ///
    /// [`crate::Module::validate`] states what a well-formed module is, and
    /// every engine that loads one checks it. Checking it *here* is what makes a
    /// violation a compiler fault at the point the bad function was emitted,
    /// rather than a loader message from whichever engine happened to run
    /// first — a dispatcher that declared one parameter and no local slots
    /// built, wrote a `.kbc`, wrote a manifest, and was reported by the hybrid
    /// loader as a manifest/bytecode arity disagreement, which names neither the
    /// function that was wrong nor what was wrong with it.
    #[error("bytecode compiler produced a module the format rejects: {0}")]
    Malformed(#[from] crate::ModuleValidateError),
}
