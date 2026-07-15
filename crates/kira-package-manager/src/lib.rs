//! Dependency resolution and package sync over manifests.
//!
//! Layer 5 of the Kira package graph.
//! Ported from kira-zig `packages/kira_package_manager`; this module tree
//! mirrors that file split so the port can land file by file.

// #![warn(missing_docs)] // enable once the port lands real code

pub mod archive;
pub mod diagnostics;
pub mod git;
pub mod manager;
pub mod paths;
pub mod progress;
pub mod registry;
pub mod registry_fetch;
pub mod types;

pub use types::{ResolvedGraph, ResolvedPackage, ResolvedPackageSource, SyncOptions};
