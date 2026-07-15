//! Fused-opcode dispatch arms: superinstructions the decode pass emits for
//! hot opcode pairs (compare+branch, load+call, ...).
//!
//! Ported from kira-zig
//! `packages/kira_vm_runtime/src/vm_interpreter_fused.zig`.
//! Logic lands with the interpreter port.
