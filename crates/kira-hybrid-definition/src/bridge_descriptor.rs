//! Bridge call descriptors between VM and native execution.
//!
//! Ported from kira-zig `kira_hybrid_definition/src/bridge_descriptor.zig`.

use kira_runtime_abi::{CallingConvention, FunctionExecution};

/// Bridge id (Zig `core.BridgeId`). Placeholder newtype.
///
/// TODO(port): replace with the `kira-core` id type once that crate defines
/// it (empty skeleton at scaffold time).
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BridgeId(pub u32);

/// Symbol id (Zig `core.SymbolId`). Placeholder newtype — same TODO as
/// [`BridgeId`].
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SymbolId(pub u32);

/// One VM<->native bridge crossing (Zig `BridgeDescriptor`).
#[derive(Debug, Clone, PartialEq)]
pub struct BridgeDescriptor {
    /// Zig `bridge_id: core.BridgeId`.
    pub bridge_id: BridgeId,
    /// Zig `function_id: core.SymbolId`.
    pub function_id: SymbolId,
    /// Zig `symbol_name: []const u8`.
    pub symbol_name: String,
    /// Zig `source_execution: FunctionExecution`.
    pub source_execution: FunctionExecution,
    /// Zig `target_execution: FunctionExecution`.
    pub target_execution: FunctionExecution,
    /// Zig `calling_convention: CallingConvention`.
    pub calling_convention: CallingConvention,
}
