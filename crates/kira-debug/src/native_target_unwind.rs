//! Stack unwinding for the native target: frame-pointer walks plus
//! DWARF CFI fallback for backtraces.
//!
//! Ported from kira-zig `packages/kira_debug/src/native_target_unwind.zig`.
//! Logic lands with the debugger port.
