//! Writes what the diagnostic code table implies, and reports when it drifts.
//!
//! Standalone tool crate (outside the layered package graph).
//!
//! Three artifacts are written from `diagnostic-codes.tsv`: the `KiraError`
//! enum a Kira program reads a diagnostic with, the function that turns code
//! text into one, and the appendix listing every code. They were maintained by
//! hand until they held 290 codes against 438 the toolchain emits, with only
//! 130 in common, so they are generated now and a test fails when they are
//! stale.

pub mod artifacts;
pub mod render;
pub mod scan;

use std::path::PathBuf;

pub use artifacts::{Artifact, artifacts};
pub use scan::emitted_codes;

/// What can stop the registry tool.
#[derive(Debug, thiserror::Error)]
pub enum RegistryError {
    /// A path under the repository root could not be read.
    #[error("{path} could not be read: {reason}")]
    Unreadable {
        /// The path that could not be read.
        path: PathBuf,
        /// What the operating system said.
        reason: String,
    },
    /// A generated artifact could not be written.
    #[error("{path} could not be written: {reason}")]
    Unwritable {
        /// The path that could not be written.
        path: PathBuf,
        /// What the operating system said.
        reason: String,
    },
}
