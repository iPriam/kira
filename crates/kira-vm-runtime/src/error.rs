//! Runtime trap types.
//!
//! These are the failures a running program can raise. Division by zero mirrors
//! the reference VM (a runtime trap, not undefined behavior). The remaining
//! variants guard invariants the typed bytecode should already guarantee; they
//! exist so a malformed module or an interpreter bug fails cleanly and typed
//! instead of panicking.

use kira_bytecode::ModuleValidateError;
use kira_runtime_abi::{
    CompilerError, FileSystemError, ForeignCallError, ForeignTypeSpec, NativeCallError,
    NativeStateError,
};

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
    UnknownFunction(u64),
    /// A host called a function with the wrong number of arguments.
    ///
    /// Only reachable through the embedder's entry point: a call the compiler
    /// emitted always matches the signature it compiled against. It surfaces a
    /// host driving the VM from an artifact that disagrees with this module.
    #[error("function {function} takes {expected} arguments, but the host passed {got}")]
    ArityMismatch {
        /// The function that was called.
        function: u64,
        /// How many parameters it declares.
        expected: u64,
        /// How many arguments the host passed.
        got: usize,
    },
    /// A host asked the VM to enter a `@Native` function.
    ///
    /// A native function's body lives in the other half of a hybrid program, so
    /// the module carries a signature and no code — bytecode may not `Call` one
    /// (validation rejects that), and neither may an embedder. Entering one
    /// would run off the end of an empty body, so it is refused by name here
    /// instead.
    #[error("function {function} is native: its body is not in this module")]
    NativeEntry {
        /// The function the host asked for.
        function: u32,
    },
    /// A `CallNative` could not be completed by the host.
    ///
    /// The VM never performs a native call itself, so it reports the host's
    /// reason verbatim rather than inventing one.
    #[error("native call failed: {0}")]
    NativeCall(NativeCallError),
    /// A `CallForeign` named a foreign-import id outside the module's table.
    ///
    /// Validation bounds the id against the foreign-import table, so this is a
    /// backstop for a module and interpreter that disagree, never a program that
    /// type-checked.
    #[error("call to unknown foreign import id {0}")]
    UnknownForeign(u32),
    /// A `ForeignCallback` named a callback id outside the module's table.
    ///
    /// Validation bounds the id against the callback table, so this is a
    /// backstop for a module and interpreter that disagree.
    #[error("unknown foreign callback id {0}")]
    UnknownForeignCallback(u32),
    /// A `CallForeign` could not be completed by the host.
    ///
    /// The VM never performs the foreign call itself, so it reports the host's
    /// typed reason verbatim — including the default `NoForeignHost` refusal a
    /// VM-only host gives.
    #[error("foreign call failed: {0}")]
    ForeignCall(ForeignCallError),
    /// A `FileSystem` instruction reached a host with no filesystem.
    ///
    /// Not a failed operation — a missing file or a refused write is an ordinary
    /// value the program reads. This is a host that cannot even be asked, which
    /// is a build-time mistake surfacing at run time.
    #[error("file-system operation failed: {0}")]
    FileSystem(FileSystemError),
    /// A `Compiler` instruction reached a host with no compiler.
    ///
    /// Not a package that failed to compile — that comes back as diagnostics the
    /// program reads. This is a host that cannot even be asked, which is what
    /// every embedded VM is: the VM sits below the compiler and can hold one
    /// only if its embedder hands it one.
    #[error("compiler operation failed: {0}")]
    Compiler(CompilerError),
    /// A Kira array held more elements than the inline C array of a
    /// `@FFI.Array` member reserves.
    ///
    /// The elements past the extent have nowhere to go, and writing only the
    /// ones that fit would hand C a different value than the program wrote. The
    /// native half traps in the same words through
    /// `kira_rt_trap_foreign_array`.
    #[error("{len} elements do not fit the inline C array of {count} at the foreign seam")]
    ForeignArrayTooLong {
        /// The C extent, in elements.
        count: u32,
        /// The Kira array's length.
        len: usize,
    },
    /// An opaque native callback-state operation failed deterministically.
    #[error("native callback state failed: {0}")]
    NativeState(NativeStateError),
    /// A callback-state instruction received a value it cannot box or name.
    ///
    /// `operation` names the instruction that refused it, because the four
    /// callback-state instructions fail for different reasons — storing a value
    /// no box can hold, recovering through something that is not a token — and
    /// a program hitting this needs to know which one it was.
    #[error("{operation} cannot take {kind}")]
    NativeStateValueMismatch {
        /// The callback-state instruction that refused the value.
        operation: NativeStateOperation,
        /// The refused shape as a noun phrase the message reads as an object
        /// ("a void value", "an array still shared with another value").
        kind: &'static str,
    },
    /// A foreign argument did not have the exact-width type its signature named.
    ///
    /// Analysis checks every foreign call's argument types, so this is a
    /// backstop: it surfaces a module whose bytecode disagrees with its
    /// foreign-import signatures, never a program that type-checked.
    #[error("foreign import {foreign} expected an argument of type {expected:?}")]
    ForeignArgMismatch {
        /// The foreign-import id at the boundary.
        foreign: u32,
        /// The exact-width type the signature named.
        expected: ForeignTypeSpec,
    },
    /// A jump target fell outside the current function's code.
    #[error("jump to out-of-range instruction {0}")]
    BadJump(u64),
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
    /// A debugger observer requested that the current run terminate.
    #[error("debugger stopped execution")]
    DebuggerStopped,
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
    /// A native call was asked to write a parameter back and did not return it.
    ///
    /// The call site's writeback targets and the callee's mutable parameters
    /// come from one IR and are written into the module and the manifest
    /// together, so this is those two artifacts disagreeing — never a program
    /// that merely type-checked. Reported rather than skipped: a writeback that
    /// quietly did not happen is a caller still holding the value it passed in,
    /// believing it was updated.
    #[error("native function {function} did not return parameter {param}, which it writes through")]
    MissingSeamWriteback {
        /// The function at the boundary.
        function: u32,
        /// The parameter slot whose final value never arrived.
        param: u64,
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
        index: u64,
    },
    /// An instruction named a local slot that cannot be represented by this
    /// host's `usize`.
    #[error("local slot {0} is too large for this host")]
    LocalSlotOutOfRange(u64),
    /// A `ConstStr` named no entry in the module string pool.
    #[error("string constant index {0} is out of range")]
    StringConstantOutOfRange(u64),
    /// A `StoreField` carried no path, so it named no field to write.
    #[error("a field store must name at least one field")]
    EmptyFieldPath,
    /// An array instruction found something other than an array.
    #[error("indexed a value that is not an array")]
    NotAnArray,
    /// A string instruction found something other than a string.
    #[error("measured a value that is not a string")]
    NotAString,
    /// `toInt` was asked for a number the text does not hold.
    ///
    /// A real program error rather than an invariant guard: `isInt` is what a
    /// program asks first, and reaching here means it did not.
    #[error("converted text that does not read as a whole number")]
    NotAWholeNumber,
    /// A member was read through a null `@FFI.Pointer`.
    ///
    /// A real program error rather than an invariant guard: `nullPointer()` is
    /// spellable, and a C callback may hand over a null for an optional
    /// argument, so the read has to check rather than trust.
    #[error("read a member through a null pointer")]
    NullForeignRead,
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
    /// A string index was outside the string.
    ///
    /// `charAt` reads one byte and `substring` carves a half-open range; both
    /// trap rather than clamp, so a program that walks off the end of a string
    /// fails the same way on every backend instead of producing a value that
    /// only one of them agrees with.
    #[error("string index is out of bounds")]
    StringIndexOutOfBounds,
    /// A `substring` was given a start past its end.
    #[error("substring range is inverted")]
    InvertedSubstring,
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
    /// A cell instruction found a slot holding something other than a cell.
    ///
    /// The analyzer boxes a `var` at its declaration and rewrites every read
    /// and write of it in the same pass, so a slot a cell instruction names
    /// always holds one — reaching this is a compiler that boxed a binding on
    /// one path and not another, never a program that merely type-checked.
    #[error("read or wrote a capture cell through a slot that does not hold one")]
    NotACell,
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
    /// A handle reached this seam, and this run has no heap that outlives it.
    ///
    /// A handle names an object in a heap that survives between calls; a `call`
    /// runs on a heap it drops at the end, so there is nothing here for the word
    /// to denote. Handles belong to the export boundary, whose persistent
    /// instance is what gives them a home — so this refuses by name rather than
    /// resolving a word into whatever object happens to sit at it.
    #[error(
        "function {function} passes a handle across the native seam, which has no heap that \
         outlives the call"
    )]
    HandleAtSeam {
        /// The function at the boundary.
        function: u32,
    },
    /// A handle named no live object in the instance it was presented to.
    ///
    /// Either the consumer released it and then used it, or it came from a
    /// different instance. A root id is never reused, so this is always a
    /// mistake about *which object* — which is exactly the mistake that must
    /// never resolve into whatever now sits at that word. See
    /// [`crate::Instance`].
    #[error("handle {root} names no live object in this instance")]
    DanglingRoot {
        /// The word the caller presented.
        root: u64,
    },
    /// An instance ran out of root ids.
    ///
    /// Root ids are never reused — that is what makes a released handle a typed
    /// error instead of a silent hit on some later object — so the supply is
    /// finite. Exhausting `u64` of them takes longer than any process runs; the
    /// variant exists so the counter converts typed rather than wrapping around
    /// onto a live root.
    #[error("this instance has no root ids left")]
    RootSpaceExhausted,
    /// A value with no crossing form reached the export boundary.
    ///
    /// Analysis refuses every uncrossable type on an `@Export` signature, so
    /// reaching this means a module and the wrapper calling it disagree — never
    /// a program that merely type-checked.
    ///
    /// The message is direction-neutral because both directions raise it: an
    /// argument the instance cannot bring in and a result it cannot hand out are
    /// the same disagreement, and `kind` is what says which.
    #[error("{kind} cannot cross the export boundary at function {function}")]
    UncrossableExport {
        /// The exported function at the boundary.
        function: u32,
        /// The offending value as a noun phrase the message reads as a subject
        /// ("an array result", "this argument").
        kind: &'static str,
    },
    /// A task primitive refused: a join that can never succeed, or a handle
    /// naming no task.
    ///
    /// The trap set is the executor's, defined once in `kira-runtime-abi`, so
    /// the VM and native code agree on *which* programs trap rather than each
    /// deciding for itself.
    #[error("task trap: {0}")]
    Task(#[from] kira_runtime_abi::TaskTrap),
}

/// Which callback-state instruction refused a value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeStateOperation {
    /// `nativeState(...)`: the value has no boxed form the host can hold.
    Store,
    /// `nativeUserData(...)`: the value is not callback state.
    UserData,
    /// `nativeRecover<T>(...)`: the value is not a callback-state token.
    Recover,
    /// Freeing callback state: the value is neither state nor a token.
    Free,
}

impl std::fmt::Display for NativeStateOperation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Store => "nativeState",
            Self::UserData => "nativeUserData",
            Self::Recover => "nativeRecover",
            Self::Free => "free",
        })
    }
}
