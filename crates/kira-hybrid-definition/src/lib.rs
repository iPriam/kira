//! Hybrid module manifests: the artifact shared by hybrid producers and
//! consumers.
//!
//! Layer 3 of the Kira package graph.
//!
//! A hybrid build splits one program across two engines, so something has to
//! describe the seam: which functions run where, what they expect, and which
//! native symbol backs each one. That description is the [`HybridManifest`],
//! written by the build and read by the hybrid runtime. Its wire format is
//! append-only once fixed.

pub mod manifest;

pub use manifest::{
    HybridForeign, HybridFunction, HybridManifest, HybridParam, MAGIC, ManifestDecodeError,
};
