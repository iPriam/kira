//! Native-call dispatch for hybrid execution: routes VM calls to bound
//! trampolines and marshals callback returns (values, native arrays, enums,
//! structs) back across the boundary.
//!
//! Ported from kira-zig
//! `packages/kira_hybrid_runtime/src/native_calls.zig`.
//! Logic lands with the bridge port.
