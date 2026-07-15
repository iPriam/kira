//! Debug session orchestration: owns the target, breakpoint table, step
//! controller, and evaluator; sequences run/stop/step/inspect verbs from the
//! REPL or DAP front end.
//!
//! Ported from kira-zig `packages/kira_debug/src/session.zig`.

use crate::breakpoint::BreakpointTable;
use crate::debug_info::Backend;
use crate::line_resolver::LineResolver;

/// One debugging session. Zig: `DebugSession`. Target/step/eval fields land
/// with their modules' ports.
#[derive(Debug)]
pub struct DebugSession {
    /// Which backend this session drives.
    pub backend: Backend,
    /// Zig: `breakpoints: BreakpointTable`.
    pub breakpoints: BreakpointTable,
    /// Zig: `line_resolver: LineResolver`.
    pub line_resolver: LineResolver,
}
