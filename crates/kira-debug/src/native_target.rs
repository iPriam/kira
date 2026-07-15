//! Native debug target: launches/attaches to LLVM-built binaries and drives
//! the platform hw controller (selected per-OS/arch; cfg-gated in the port,
//! mirroring Zig's comptime platform switch so off-platform controllers are
//! never compiled).
//!
//! Ported from kira-zig `packages/kira_debug/src/native_target.zig`.
//! Logic lands with the debugger port. Known platform wall carried from the
//! Zig side: on Apple Silicon, cross-process
//! `thread_set_state(ARM_DEBUG_STATE64)` hangs and code pages are
//! unwritable — see the darwin hw modules.
