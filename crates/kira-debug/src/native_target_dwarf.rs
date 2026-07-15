//! DWARF consumption for the native target: locates debug info (dSYM on
//! macOS, .debug_* sections elsewhere), reads line programs and variable
//! locations.
//!
//! Ported from kira-zig `packages/kira_debug/src/native_target_dwarf.zig`.
//! Logic lands with the debugger port.
