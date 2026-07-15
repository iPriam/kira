//! VM side of the native bridge: the runtime-invoker installed into native
//! code, argument/result marshaling at the boundary, and boundary pin scopes.
//!
//! Ported from kira-zig
//! `packages/kira_vm_runtime/src/vm_native_bridge.zig` (regression tests stay
//! test-side). Logic lands with the bridge port.
