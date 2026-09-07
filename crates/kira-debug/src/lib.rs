//! Kira's debugger model and engine adapters.
//!
//! Layer 4 of the Kira package graph. The debugger has one source-level model
//! and three execution adapters:
//!
//! - the VM reports instruction stops without adding work to an ordinary run,
//!   and can expose those stops through its exported LLDB probe;
//! - the hybrid host reports the same VM stops while native functions remain
//!   visible as stable trampoline symbols;
//! - LLVM emits DWARF plus the host's native debug companion, and the launcher
//!   can ask LLDB for the target's actual CPU instructions.

mod dap;
mod engine;
mod lldb;
mod model;
mod target;
mod vm;
mod vm_state;

pub use dap::{
    DEFAULT_TIMEOUT, DapClient, DapError, LldbDapBreakpoint, LldbDapLaunch, Stop, TargetState,
    TransportError, decode_base64, parse_address,
};
pub use engine::{
    ENABLE_DEBUGGING_HINT, Engine, LLDB_DAP_VARIABLE, LLDB_VARIABLE, configure as configure_engine,
    debugging_unauthorized, support_directories,
};
pub use lldb::{LldbError, LldbLaunch, LldbOutput, target_label};
pub use model::{Backend, DebugFunction, DebugInfo, DebugSource, function_symbol};
pub use target::{
    Execution, PreparedFunction, PreparedTarget, VM_PROBE_SYMBOL, VM_TEXT_SYMBOL, VmProbe,
    probe_registers,
};
pub use vm::{Breakpoint, VmDebugger, VmDebuggerMode};
pub use vm_state::{VmFrame, VmStop, VmValue};
