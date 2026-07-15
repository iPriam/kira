//! VM-side debug controller: breakpoint checks in the dispatch loop, stop
//! reporting, and local/frame inspection hooks consumed by kira-debug.
//!
//! Ported from kira-zig `packages/kira_vm_runtime/src/vm_debug.zig`.
//! Logic lands with the debugger port.
