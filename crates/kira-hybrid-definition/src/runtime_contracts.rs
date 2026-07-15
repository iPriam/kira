//! Hybrid runtime entry contracts.
//!
//! Ported from kira-zig `kira_hybrid_definition/src/runtime_contracts.zig`.

use kira_runtime_abi::FunctionExecution;

/// The entry contract a hybrid module hands the runtime (Zig `RuntimeContract`).
#[derive(Debug, Clone, PartialEq)]
pub struct RuntimeContract {
    /// Zig `module_name: []const u8`.
    pub module_name: String,
    /// Zig `entry_function_id: u32`.
    pub entry_function_id: u32,
    /// Zig `entry_execution: FunctionExecution`.
    pub entry_execution: FunctionExecution,
}
