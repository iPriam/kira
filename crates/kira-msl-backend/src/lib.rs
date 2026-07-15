//! Lowers shader IR to MSL (Metal Shading Language).
//!
//! Layer 4 of the Kira package graph.
//! Ported from kira-zig `packages/kira_msl_backend` (`msl.zig`).

// #![warn(missing_docs)] // enable once the port lands real code

pub mod msl;

pub use msl::{LoweredShader, MslLowerer};

/// Returns this crate's name (scaffold smoke check).
pub fn crate_name() -> &'static str {
    "kira-msl-backend"
}
