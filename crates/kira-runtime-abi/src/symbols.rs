//! Well-known runtime symbol ids.
//!
//! Ported from kira-zig `kira_runtime_abi/src/symbols.zig`.

/// Built-in runtime symbols addressable by id (Zig `RuntimeSymbol`, `enum(u32)`).
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RuntimeSymbol {
    /// Zig `.print = 1`.
    Print = 1,
}
