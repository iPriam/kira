//! Hot-swap compatibility checks: decides whether a staged swap is safe to
//! apply in place by diffing struct/enum layouts and the signatures of
//! functions referenced by live closures against the retiring module.
//!
//! Ported from kira-zig
//! `packages/kira_hybrid_runtime/src/hot_swap_compat.zig`.
//! Logic lands with the live port.
