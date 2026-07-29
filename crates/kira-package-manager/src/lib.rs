//! Dependency resolution and package sync over manifests.
//!
//! Layer 5 of the Kira package graph.

pub mod graph;
pub mod resolver;

mod lockfile_check;
mod lockfile_sync;

pub use graph::{LockfileStatus, ResolvedDependency, ResolvedPackage, ResolvedPackageGraph};
pub use lockfile_sync::{LockfileError, SyncOutcome, render_lockfile, sync_lockfile};
pub use resolver::{ResolveError, resolve};
