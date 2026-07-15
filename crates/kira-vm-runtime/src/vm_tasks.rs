//! The VM task runtime: task control blocks, the ready/suspended queues,
//! lazy spawn, cancel-prevents-run, join/detach, and drain-all shutdown.
//!
//! Ported from kira-zig `packages/kira_vm_runtime/src/vm_tasks.zig`.
//! Logic lands with the async-spine port.
