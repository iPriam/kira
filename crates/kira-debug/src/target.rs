//! The `DebugTarget` contract every backend implements: launch/attach,
//! resume/pause, step kinds, breakpoint arm/disarm, frame and local reads,
//! and the shared `TargetError` set.
//!
//! Ported from kira-zig `packages/kira_debug/src/target.zig`.
//! The trait definition lands with the debugger port.
