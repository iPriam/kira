//! Shared debug-info contract: source positions/spans, frames, local views,
//! stop reasons, breakpoint specs, and hardware capability descriptors —
//! the vocabulary every target backend and the session speak.
//!
//! Ported from kira-zig `packages/kira_debug/src/debug_info.zig`.
//! Scaffold: the small identity types are ported; `Frame`, `LocalView`,
//! `HwCapabilities`, and `WatchKind` land with the debugger port.

/// A 1-based source position. Zig: `SourcePosition`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct SourcePosition {
    pub line: u32,
    pub column: u32,
}

/// A source span between two positions. Zig: `SourceSpan`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct SourceSpan {
    pub start: SourcePosition,
    pub end: SourcePosition,
}

/// Which runtime a debug target drives. Zig: `Backend`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Backend {
    Vm,
    Native,
    Hybrid,
}

/// Why the target stopped. Zig: `StopReason` (scaffold subset; the full
/// set — watchpoints, signals, task traps — lands with the port).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StopReason {
    Breakpoint { id: u32 },
    Step,
    Entry,
    Exited { code: i32 },
}

/// A user breakpoint request. Zig: `BreakpointSpec`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BreakpointSpec {
    pub file: String,
    pub line: u32,
}
