//! The desktop runner's library face.
//!
//! A re-export of [`kira-bundle-host`], the crate that actually hosts
//! bundles. The lib target exists so a workspace build of `kira-cli` also
//! builds this crate's binary (cargo never builds a dependency's `[[bin]]`,
//! but it always builds its lib), and so existing consumers of the runner's
//! types keep one import path. New code should depend on `kira-bundle-host`
//! directly.

pub use kira_bundle_host as host;
pub use kira_bundle_host::{BundleHost, BundleHostError};
