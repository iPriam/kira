//! Call prologue/epilogue: frame setup, argument transfer into the callee
//! register file, and owned-slot bookkeeping on entry/exit.
//!
//! Ported from kira-zig
//! `packages/kira_vm_runtime/src/vm_interpreter_prologue.zig`.
//! Logic lands with the interpreter port.
