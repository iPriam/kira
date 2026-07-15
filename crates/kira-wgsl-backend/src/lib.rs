//! Lowers shader IR to WGSL.
//!
//! Layer 4 of the Kira package graph.
//! Ported from kira-zig `packages/kira_wgsl_backend` (`wgsl.zig`).

// #![warn(missing_docs)] // enable once the port lands real code

pub mod wgsl;

pub use wgsl::{LoweredShader, WgslLowerer};

/// Returns this crate's name (scaffold smoke check).
pub fn crate_name() -> &'static str {
    "kira-wgsl-backend"
}
