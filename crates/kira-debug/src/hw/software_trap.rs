//! Software-trap fallback controller: patches trap instructions
//! (int3/brk) when hardware slots are exhausted or unavailable.
//!
//! Ported from kira-zig `packages/kira_debug/src/hw/software_trap.zig`.
//! No code-patching at scaffold time.
