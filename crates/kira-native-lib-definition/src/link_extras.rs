//! Extra link inputs attached to a native library target.
//!
//! Ported from kira-zig `kira_native_lib_definition/src/link_extras.zig`.

/// Extra link inputs (Zig `LinkExtras`).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct LinkExtras {
    /// Zig `include_dirs: []const []const u8`.
    pub include_dirs: Vec<String>,
    /// Zig `defines: []const []const u8`.
    pub defines: Vec<String>,
    /// Zig `frameworks: []const []const u8`.
    pub frameworks: Vec<String>,
    /// Zig `system_libs: []const []const u8`.
    pub system_libs: Vec<String>,
    /// Zig `linker_flags` — raw flags appended verbatim to the final
    /// program/library link command (e.g. `--use-port=emdawnwebgpu`,
    /// `-sASYNCIFY`). Target-scoped and generic across every backend.
    pub linker_flags: Vec<String>,
}
