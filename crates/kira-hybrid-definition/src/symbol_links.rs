//! Function-id to exported-symbol links.
//!
//! Ported from kira-zig `kira_hybrid_definition/src/symbol_links.zig`.

/// Links a function id to its exported native symbol (Zig `SymbolLink`).
#[derive(Debug, Clone, PartialEq)]
pub struct SymbolLink {
    /// Zig `function_id: u32`.
    pub function_id: u32,
    /// Zig `exported_name: []const u8`.
    pub exported_name: String,
}
