//! The `HybridRuntime` container: one VM plus one bound native library
//! executing a hybrid module (bytecode UI logic + LLVM-compiled hot
//! functions), with live hot-reload state and pending native callback
//! returns.
//!
//! Ported from kira-zig `packages/kira_hybrid_runtime/src/runtime.zig`.

use kira_native_bridge::NativeBridge;
use kira_vm_runtime::Vm;

use crate::hot_swap::ReloadState;

/// A running hybrid module.
///
/// Zig: `HybridRuntime` (allocator, manifest, bytecode module, VM, bridge,
/// reload state, pending-callback return tracking). Scaffold: manifest and
/// bytecode module fields land once kira-hybrid-definition / kira-bytecode
/// scaffold their types (TODO(port)).
#[derive(Debug, Default)]
pub struct HybridRuntime {
    /// Zig: `vm: vm_runtime.Vm`.
    pub vm: Vm,
    /// Zig: `bridge: native_bridge.NativeBridge`.
    pub bridge: NativeBridge,
    /// Live hot-reload state (staged module swap + retired programs); inert
    /// unless a live runner stages a swap. Zig: `reload: hot_swap.ReloadState`.
    pub reload: ReloadState,
    // TODO(port): manifest (kira-hybrid-definition), module (kira-bytecode),
    // pending_callback_return_values / _native_arrays / _native_enums /
    // _native_structs lists once the ABI value types settle in
    // kira-runtime-abi.
}
