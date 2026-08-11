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

mod lldb;
mod lldb_dap;
mod model;
mod vm;

pub use lldb::{LldbError, LldbLaunch, LldbOutput, target_label};
pub use lldb_dap::{LldbDapBreakpoint, LldbDapLaunch};
pub use model::{Backend, DebugFunction, DebugInfo, DebugSource, function_symbol};
pub use vm::{Breakpoint, VmDebugger, VmDebuggerMode};
