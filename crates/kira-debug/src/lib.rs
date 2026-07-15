//! Source-level debugger core, backend-agnostic across VM, native
//! (hardware-assisted), and hybrid.
//!
//! Layer 4 of the Kira package graph.
//! Ported from kira-zig `packages/kira_debug`; the module tree mirrors the
//! Zig file split one-to-one (test files excluded), including the `hw/`
//! platform controllers and the `protocol/` wire protocols.

// #![warn(missing_docs)] // enable once the port lands real code

// Contract / foundation modules.
pub mod debug_info;
pub mod hw;
pub mod target;

// Breakpoint / stepping / frame model.
pub mod breakpoint;
pub mod frame;
pub mod line_resolver;
pub mod step;
pub mod sync;
pub mod value_view;

// Expression evaluation.
pub mod eval;
pub mod eval_parse;
pub mod eval_types;

// Backend targets. Each imports only its own backend's runtime; the native
// target selects the current-platform hw controller (cfg-gated in the port,
// mirroring Zig's comptime switch).
pub mod hybrid_target;
pub mod native_target;
pub mod native_target_dwarf;
pub mod native_target_launch;
pub mod native_target_unwind;
pub mod vm_target;
pub mod vm_target_locals;

// Session orchestration + REPL + wire protocols.
pub mod protocol;
pub mod repl;
pub mod session;

// Re-export the most-used types at the crate root for ergonomics
// (mirrors the Zig root.zig re-export block).
pub use breakpoint::BreakpointTable;
pub use debug_info::{Backend, BreakpointSpec, SourcePosition, SourceSpan, StopReason};
pub use line_resolver::LineResolver;
pub use session::DebugSession;

/// Returns this crate's name (scaffold smoke check).
pub fn crate_name() -> &'static str {
    "kira-debug"
}
