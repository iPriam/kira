//! Project model: resolved projects, package roots, and build targets.
//!
//! Layer 5 of the Kira package graph.

pub mod package_discovery;
pub mod project;
pub mod workspace;

pub use package_discovery::{
    DECLARATION_MANIFEST_FILE_NAME, DiscoveryError, ENTRYPOINT_REL_PATH, LEGACY_MANIFEST_FILE_NAME,
    LibrarySource, LibrarySources, MANIFEST_FILE_NAME, MANIFEST_FILE_NAMES, Manifest,
    PREFERRED_MANIFEST_FILE_NAME, REPO_MANIFEST_FILE_NAME, library_sources,
    library_sources_for_entry, manifest_for, resolve_target,
};
pub use project::{
    CommandMode, Project, ResolvedPackageRoot, ResolvedProject, ResolvedTarget, TargetKind,
};
pub use workspace::Workspace;
