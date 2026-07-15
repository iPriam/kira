//! Calling conventions and execution-mode selectors.
//!
//! Ported from kira-zig `kira_runtime_abi/src/calling.zig`.

/// How a callee expects to be invoked (Zig `CallingConvention`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum CallingConvention {
    /// Zig `.c` — plain C ABI (FFI default).
    #[default]
    C,
    /// Zig `.kira_vm` — VM-interpreted Kira function.
    KiraVm,
    /// Zig `.kira_hybrid` — hybrid-bridged Kira function.
    KiraHybrid,
}

/// Where a function's body executes (Zig `FunctionExecution`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum FunctionExecution {
    /// Zig `.inherited` — follow the enclosing module/program mode.
    #[default]
    Inherited,
    /// Zig `.runtime` — VM-interpreted.
    Runtime,
    /// Zig `.native` — LLVM-compiled native code.
    Native,
}

/// Whole-program execution mode (Zig `ExecutionMode`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ExecutionMode {
    /// Zig `.vm` — pure bytecode interpretation.
    Vm,
    /// Zig `.llvm_native` — fully LLVM-compiled native.
    LlvmNative,
    /// Zig `.hybrid` — mixed VM + native with bridge calls.
    Hybrid,
}
