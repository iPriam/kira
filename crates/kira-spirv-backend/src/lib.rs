//! Lowers shader IR to SPIR-V.
//!
//! Layer 4 of the Kira package graph.
//! Ported from kira-zig `packages/kira_spirv_backend` (`spirv.zig`).

// #![warn(missing_docs)] // enable once the port lands real code

pub mod spirv;

pub use spirv::{LoweredShader, SpirvLowerer};

/// Returns this crate's name (scaffold smoke check).
pub fn crate_name() -> &'static str {
    "kira-spirv-backend"
}
