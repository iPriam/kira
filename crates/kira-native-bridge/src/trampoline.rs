//! Per-function trampoline: the resolved native entry for one bytecode
//! function id.
//!
//! Ported from kira-zig `packages/kira_native_bridge/src/trampoline.zig`.

use crate::abi::KiraBridgeValue;

/// The uniform native entry signature every compiled Kira function exposes
/// to the bridge. Zig: `NativeTrampolineFn`
/// (`fn(?[*]const BridgeValue, u32, *BridgeValue) callconv(.c) void`).
pub type NativeTrampolineFn =
    unsafe extern "C" fn(args: *const KiraBridgeValue, arg_count: u32, out: *mut KiraBridgeValue);

/// One bound trampoline. Zig: `Trampoline`.
#[derive(Debug, Clone)]
pub struct Trampoline {
    /// Zig: `function_id: u32`.
    pub function_id: u32,
    /// Zig: `symbol_name: []const u8`.
    pub symbol_name: String,
    /// Zig: `invoke: NativeTrampolineFn`.
    pub invoke: NativeTrampolineFn,
}
