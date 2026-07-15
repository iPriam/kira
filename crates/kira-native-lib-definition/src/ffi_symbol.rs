//! FFI symbol declarations.
//!
//! Ported from kira-zig `kira_native_lib_definition/src/ffi_symbol.zig`.

use kira_runtime_abi::CallingConvention;

/// A native symbol exposed by a library (Zig `NativeSymbol`).
#[derive(Debug, Clone, PartialEq)]
pub struct NativeSymbol {
    /// Zig `name: []const u8` — the Kira-visible name.
    pub name: String,
    /// Zig `symbol_name: []const u8` — the linker symbol.
    pub symbol_name: String,
    /// Zig `calling_convention: CallingConvention = .c`.
    pub calling_convention: CallingConvention,
}
