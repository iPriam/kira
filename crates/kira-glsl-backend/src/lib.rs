//! Lowers shader IR to GLSL.
//!
//! Layer 4 of the Kira package graph.
//! Ported from kira-zig `packages/kira_glsl_backend` (`glsl.zig`).

// #![warn(missing_docs)] // enable once the port lands real code

pub mod glsl;

pub use glsl::{GlslLowerer, LoweredShader};

/// Returns this crate's name (scaffold smoke check).
pub fn crate_name() -> &'static str {
    "kira-glsl-backend"
}
