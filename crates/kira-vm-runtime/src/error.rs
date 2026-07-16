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
}
