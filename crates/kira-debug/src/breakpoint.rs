//! Breakpoint bookkeeping: id allocation, spec -> resolved-location binding,
//! and enable/disable state, shared by all target backends.
//!
//! Ported from kira-zig `packages/kira_debug/src/breakpoint.zig`.

use crate::debug_info::BreakpointSpec;

/// The breakpoint table. Zig: `BreakpointTable` (id counter + entries with
/// resolved code locations per backend). Resolution/arming logic lands with
/// the port.
#[derive(Debug, Default)]
pub struct BreakpointTable {
    /// Next breakpoint id to hand out.
    pub next_id: u32,
    /// Registered breakpoints, keyed by id.
    pub entries: Vec<(u32, BreakpointSpec)>,
}
