//! Unbuffered stdout writer used for hybrid print parity with native
//! builds (bytes hit the pipe in the same order/granularity as the C
//! helpers' writes).
//!
//! Ported from kira-zig
//! `packages/kira_hybrid_runtime/src/direct_stdout_writer.zig`.
//! Logic lands with the runtime port.
