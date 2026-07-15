//! Hybrid runtime: loads hybrid modules, binds symbols, and hot-swaps modules in place.
//!
//! Layer 4 of the Kira package graph.
//! Ported from kira-zig `packages/kira_hybrid_runtime`. The module tree
//! mirrors the Zig file split one-to-one.

// #![warn(missing_docs)] // enable once the port lands real code

pub mod binder;
pub mod direct_stdout_writer;
pub mod hot_swap;
pub mod hot_swap_compat;
pub mod loader;
pub mod native_calls;
pub mod runtime;

pub use hot_swap::{ReloadEvent, ReloadState, StagedSwap};
pub use runtime::HybridRuntime;

/// Returns this crate's name (scaffold smoke check).
pub fn crate_name() -> &'static str {
    "kira-hybrid-runtime"
}
