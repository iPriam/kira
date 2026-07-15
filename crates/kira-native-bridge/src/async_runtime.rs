//! The C task runtime backing `kira_task_*` exports for native builds:
//! task control blocks, worker threads, spawn/await/cancel/detach, and
//! drain-all shutdown — mirroring the VM task semantics exactly.
//!
//! Ported from kira-zig
//! `packages/kira_native_bridge/src/async_runtime.zig` (plus
//! `runtime_helpers_tasks.inc`). Logic lands with the async-spine port.
