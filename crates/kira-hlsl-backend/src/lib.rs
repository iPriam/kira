//! Lowers shader IR to HLSL.
//!
//! Layer 4 of the Kira package graph.
//! Ported from kira-zig `packages/kira_hlsl_backend` (`hlsl.zig`).

// #![warn(missing_docs)] // enable once the port lands real code

pub mod hlsl;

pub use hlsl::{HlslLowerer, LoweredShader};

/// Returns this crate's name (scaffold smoke check).
pub fn crate_name() -> &'static str {
    "kira-hlsl-backend"
}
