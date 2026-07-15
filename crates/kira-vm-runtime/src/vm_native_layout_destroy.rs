//! Destructors for native-layout values: releases materialized arrays,
//! structs, and strings produced by `native_layout` without double-freeing
//! bridge-owned memory.
//!
//! Ported from kira-zig
//! `packages/kira_vm_runtime/src/vm_native_layout_destroy.zig`.
//! Logic lands with the bridge port.
