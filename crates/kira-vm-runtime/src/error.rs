//! Runtime trap types.
//!
//! These are the failures a running program can raise. Division by zero mirrors
//! the reference VM (a runtime trap, not undefined behavior). The remaining
//! variants guard invariants the typed bytecode should already guarantee; they
//! exist so a malformed module or an interpreter bug fails cleanly and typed
//! instead of panicking.

use kira_bytecode::ModuleValidateError;
use kira_runtime_abi::NativeCallError;

/// A trap raised while executing bytecode.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum VmError {
    /// The module failed structural validation before execution began.
    #[error("invalid module: {0}")]
    Module(#[from] ModuleValidateError),
    /// Execution was asked to run a module that carries no entrypoint.
    ///
    /// A library module is well-formed and validates cleanly; it simply has
    /// nothing to start. Reported by name rather than as a validation failure,
    /// because the module is not broken — the request was.
    #[error("this module is a library: it has no entrypoint to run")]
    NoEntrypoint,
    /// Integer division or remainder by zero.
    #[error("vm divide does not allow division by zero")]
    DivideByZero,
    /// The operand stack was empty when a value was expected.
    #[error("operand stack underflow")]
    StackUnderflow,
    /// An operand had a type the instruction did not expect.
    #[error("type mismatch: expected {expected} on the operand stack")]
    TypeMismatch {
        /// The type the instruction required.
        expected: &'static str,
    },
    /// A `Call` named a function index outside the module.
    #[error("call to unknown function index {0}")]
    UnknownFunction(u32),
    /// A host called a function with the wrong number of arguments.
    ///
    /// Only reachable through the embedder's entry point: a call the compiler
    /// emitted always matches the signature it compiled against. It surfaces a
    /// host driving the VM from an artifact that disagrees with this module.
    #[error("function {function} takes {expected} arguments, but the host passed {got}")]
    ArityMismatch {
        /// The function that was called.
        function: u32,
        /// How many parameters it declares.
        expected: u16,
        /// How many arguments the host passed.
        got: usize,
    },
    /// A `CallNative` could not be completed by the host.
    ///
    /// The VM never performs a native call itself, so it reports the host's
    /// reason verbatim rather than inventing one.
    #[error("native call failed: {0}")]
    NativeCall(NativeCallError),
    /// A jump target fell outside the current function's code.
    #[error("jump to out-of-range instruction {0}")]
    BadJump(u32),
    /// Recursion or looping exceeded the interpreter's call-depth guard.
    #[error("maximum call depth exceeded")]
    CallDepthExceeded,
    /// The call-frame stack emptied unexpectedly (an interpreter bug, not a
    /// program error).
    #[error("internal fault: call frame stack empty")]
    FrameUnderflow,
    /// Instruction routing reached an impossible combination (an interpreter
    /// bug, not a program error).
    #[error("internal fault: instruction dispatch invariant violated")]
    BadDispatch,
    /// A struct reached the native seam, which has no layout for one.
    ///
    /// The hybrid split rejects a struct in a `@Native` signature when the
    /// program is built, so reaching this means a module and a manifest that
    /// disagree — never a program that merely type-checked.
    #[error(
        "function {function} passes a struct across the native seam, which has no layout for one"
    )]
    StructAtSeam {
        /// The function at the boundary.
        function: u32,
    },
    /// `print` was handed a value with no pinned rendering (a struct).
    ///
    /// Analysis rejects this before a program runs; it is a trap rather than
    /// invented output.
    #[error("print cannot format this value")]
    UnprintableValue,
    /// A field instruction found something other than a struct.
    #[error("field access on a value that is not a struct")]
    NotAStruct,
    /// A field index named no field of the struct in hand.
    #[error("no field at index {index}")]
    NoSuchField {
        /// The index the instruction asked for.
        index: u16,
    },
    /// A `StoreField` carried no path, so it named no field to write.
    #[error("a field store must name at least one field")]
    EmptyFieldPath,
    /// An array instruction found something other than an array.
    #[error("indexed a value that is not an array")]
    NotAnArray,
    /// An array index was at or past the end.
    ///
    /// A real program error, not an invariant guard: an index is generally not
    /// a constant, so this is checked when the program runs rather than when it
    /// is compiled. Kept **distinct** from [`VmError::NegativeIndex`] because
    /// they are distinct mistakes — a length misjudged versus a computation
    /// that went wrong — and one message for both would say less.
    #[error("array index is out of bounds")]
    IndexOutOfBounds,
    /// An array index was negative.
    ///
    /// The message does not name the offending index, and deliberately: a wasm
    /// trap path cannot format one without allocating a string mid-trap, and a
    /// trap that reads differently on one backend is not the same trap. The
    /// oracle does not name it either, so naming it here would have been
    /// invented detail bought at the price of parity.
    #[error("array index is negative")]
    NegativeIndex,
    /// An array reached the native seam, which has no layout for one *yet*.
    ///
    /// Unlike [`VmError::StructAtSeam`], this one is a gap rather than a
    /// decision: the language does let an array cross. Carrying one needs an
    /// ownership answer at the boundary — who frees the elements, and what a
    /// native function growing the array means for the VM's heap — that this
    /// port has not made. Refusing it is what keeps the alternative (a double
    /// free or a leak at the boundary) from shipping. See
    /// `.codex/work/arrays.md`.
    #[error(
        "function {function} passes an array across the native seam, which cannot carry one yet"
    )]
    ArrayAtSeam {
        /// The function at the boundary.
        function: u32,
    },
    /// An array holds more elements than `Int` can count.
    ///
    /// Only reachable on a 64-bit host by an array of more than `i64::MAX`
    /// elements, which cannot be allocated — it exists so `.count` converts
    /// typed rather than with an unwrap.
    #[error("array is too long to count in an Int")]
    ArrayTooLong,
    /// A tag instruction found something other than an enum.
    #[error("read a tag from a value that is not an enum")]
    NotAnEnum,
    /// A payload projection found a variant carrying no payload.
    ///
    /// A `match` only projects inside the arm its tag test selected, so the
    /// variant is always the one whose payload the binding was typed against —
    /// reaching this is a compiler that emitted the projection under the wrong
    /// tag test, never a program that merely type-checked.
    #[error("read a payload from an enum variant that carries none")]
    MissingEnumPayload,
    /// An enum reached the native seam, which has no layout for one.
    ///
    /// Like a struct, an enum has no representation in the hybrid ABI, and the
    /// split is checked when the program is built — so reaching this is a module
    /// and a manifest that disagree, never a program that merely type-checked.
    #[error(
        "function {function} passes an enum across the native seam, which has no layout for one"
    )]
    EnumAtSeam {
        /// The function at the boundary.
        function: u32,
    },
}
