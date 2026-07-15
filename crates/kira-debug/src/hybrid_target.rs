//! Hybrid debug target: composes the VM target (bytecode frames) with the
//! native target (LLVM-compiled frames) over one `HybridRuntime`.
//!
//! Ported from kira-zig `packages/kira_debug/src/hybrid_target.zig`.
//! Logic lands with the debugger port.
