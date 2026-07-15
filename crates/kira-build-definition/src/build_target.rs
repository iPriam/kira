//! Execution targets and their capabilities.
//!
//! Ported from kira-zig `kira_build_definition/src/build_target.zig`.

use kira_native_lib_definition::TargetSelector;

/// Where the built program executes (Zig `ExecutionTarget`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ExecutionTarget {
    /// Zig `.vm`.
    #[default]
    Vm,
    /// Zig `.llvm_native`.
    LlvmNative,
    /// Zig `.wasm32_emscripten`.
    Wasm32Emscripten,
    /// Zig `.hybrid`.
    Hybrid,
}

impl ExecutionTarget {
    /// The environment the target runs in (Zig `environment`).
    pub fn environment(self) -> TargetEnvironment {
        match self {
            ExecutionTarget::Vm | ExecutionTarget::LlvmNative | ExecutionTarget::Hybrid => {
                TargetEnvironment::HostNative
            }
            ExecutionTarget::Wasm32Emscripten => TargetEnvironment::Browser,
        }
    }

    /// What the target can do (Zig `capabilities`).
    pub fn capabilities(self) -> TargetCapabilities {
        match self {
            ExecutionTarget::Vm => TargetCapabilities {
                host_native_libraries: false,
                browser_host_bindings: false,
                executes_in_browser_sandbox: false,
            },
            ExecutionTarget::LlvmNative | ExecutionTarget::Hybrid => TargetCapabilities {
                host_native_libraries: true,
                browser_host_bindings: false,
                executes_in_browser_sandbox: false,
            },
            ExecutionTarget::Wasm32Emscripten => TargetCapabilities {
                host_native_libraries: false,
                browser_host_bindings: true,
                executes_in_browser_sandbox: true,
            },
        }
    }
}

/// A build target: execution mode plus optional concrete triple (Zig `BuildTarget`).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct BuildTarget {
    /// Zig `execution: ExecutionTarget = .vm`.
    pub execution: ExecutionTarget,
    /// Zig `selector: ?TargetSelector`.
    pub selector: Option<TargetSelector>,
}

/// Host environment family (Zig `TargetEnvironment`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TargetEnvironment {
    /// Zig `.host_native`.
    HostNative,
    /// Zig `.browser`.
    Browser,
}

/// Capability flags per execution target (Zig `TargetCapabilities`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TargetCapabilities {
    /// Zig `host_native_libraries: bool`.
    pub host_native_libraries: bool,
    /// Zig `browser_host_bindings: bool`.
    pub browser_host_bindings: bool,
    /// Zig `executes_in_browser_sandbox: bool`.
    pub executes_in_browser_sandbox: bool,
}
