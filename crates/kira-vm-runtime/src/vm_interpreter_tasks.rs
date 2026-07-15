//! Interpreter arms for task opcodes (KBCC): spawn/await/cancel/detach and
//! the trap semantics of joining a failed task.
//!
//! Ported from kira-zig
//! `packages/kira_vm_runtime/src/vm_interpreter_tasks.zig`.
//! Logic lands with the async-spine port.
