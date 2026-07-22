//! Project model: resolved projects, package roots, and build targets.
//!
//! Layer 5 of the Kira package graph.

pub mod native_libraries;
pub mod package_discovery;
pub mod project;
pub mod workspace;

pub use native_libraries::{
    NativeLibraryResolveError, NativeLinkResolution, resolve_native_libraries,
};
pub use package_discovery::{
    BIND_TYPES_DIR_NAME, BIND_TYPES_FILE_SUFFIX, DECLARATION_MANIFEST_FILE_NAME, DiscoveryError,
    ENTRYPOINT_REL_PATH, LEGACY_MANIFEST_FILE_NAME, LibrarySource, LibrarySources,
    MANIFEST_FILE_NAME, MANIFEST_FILE_NAMES, Manifest, PREFERRED_MANIFEST_FILE_NAME,
    REPO_MANIFEST_FILE_NAME, is_misplaced_bind_types_file, library_sources,
    library_sources_for_entry, manifest_for, resolve_target,
};
pub use project::{
    CommandMode, Project, ResolvedPackageRoot, ResolvedProject, ResolvedTarget, TargetKind,
};
pub use workspace::Workspace;
