//! Native process launch/attach plumbing: spawn-stopped, exec redirection,
//! and initial rendezvous with the hw controller.
//!
//! Ported from kira-zig `packages/kira_debug/src/native_target_launch.zig`.
//! Logic lands with the debugger port.
