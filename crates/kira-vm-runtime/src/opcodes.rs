//! KBC opcode definitions: the opcode enum, operand encodings, and decode
//! helpers shared by the interpreter and the debugger.
//!
//! Ported from kira-zig `packages/kira_vm_runtime/src/opcodes.zig`.
//! TODO(port): the opcode set likely moves to (or is re-exported from)
//! kira-bytecode; keep one source of truth when both land.
