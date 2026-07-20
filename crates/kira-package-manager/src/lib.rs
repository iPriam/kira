//! Dependency resolution and package sync over manifests.
//!
//! Layer 5 of the Kira package graph.

pub mod graph;
pub mod resolver;

mod lockfile_check;

pub use graph::{ResolvedPackage, ResolvedPackageGraph};
pub use resolver::{ResolveError, resolve};
