//! Maps source file:line requests to executable locations: KBCD line-table
//! entries for the VM, DWARF line rows (via dSYM on macOS) for native —
//! including nearest-line snapping.
//!
//! Ported from kira-zig `packages/kira_debug/src/line_resolver.zig`.

/// The line resolver. Zig: `LineResolver` (loaded line tables per module /
/// compilation unit). Lookup logic lands with the port.
#[derive(Debug, Default)]
pub struct LineResolver {
    /// Source files with loaded line tables (paths as loaded).
    pub loaded_files: Vec<String>,
}
