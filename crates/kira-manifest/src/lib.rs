//! package.kira manifests, dependency specs, and lock files.
//!
//! Layer 5 of the Kira package graph.
//!
//! Scaffolding status: the manifest model types (this crate's public surface)
//! are defined; the loaders/writers/parsers are module stubs that fill in as
//! the crate grows.

pub mod declaration_loader;
pub mod declaration_loader_state;
pub mod declaration_writer;
pub mod dependency;
pub mod lockfile;
pub mod native_lib_manifest;
pub mod native_lib_parser;
pub mod package_manifest;
pub mod parser;
pub mod platform_config;
pub mod project_manifest;
pub mod tests_config;
pub mod toml_text;

pub use declaration_loader::{DeclarationError, load as load_declaration};
pub use dependency::{DependencySource, DependencySpec, GitSource, PathSource, RegistrySource};
pub use lockfile::LockFile;
pub use package_manifest::PackageManifest;
pub use platform_config::{
    ApplePlatform, Backend, BackendSelectionSource, BuildProfile, BuildSystem, ExecutionBackend,
    ExecutionPolicy, ExportFamily, HybridSelectionMode, LibraryExecutionPolicy, LiveProtocolMode,
    ProfileConfig, ResolvedConfig, RunnerConfig, RunnerId, WebExecutionPolicy, WebGraphicsBridge,
    WebGraphicsCapability, WebRenderingModel, WebSurface, WebSurfaceRequirements,
    default_resolved_config, web_surface_requirements,
};
pub use project_manifest::{PackageKind, ProjectManifest};
pub use tests_config::{TestPhase, TestsConfig};
