//! package.kira manifests, dependency specs, and lock files.
//!
//! Layer 5 of the Kira package graph.

pub mod declaration_loader;
mod declaration_native_libs;
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
pub use declaration_writer::{
    DeclarationWriteError, render as render_declaration, write as write_declaration,
};
pub use dependency::{
    DependencyMutationError, DependencySource, DependencySpec, GitSource, PathSource,
    RegistrySource,
};
pub use lockfile::LockFile;
pub use native_lib_manifest::{
    RawFlatManifest, RawFlatTarget, RawSectionedManifest, RawSectionedTarget,
};
pub use native_lib_parser::{NativeLibParseError, parse_native_lib_manifest};
pub use package_manifest::PackageManifest;
pub use parser::{
    LegacyManifestError, load_legacy_manifest, render_legacy_manifest, write_legacy_manifest,
};
pub use platform_config::{
    ApplePlatform, Backend, BackendSelectionSource, BuildProfile, BuildSystem, ExecutionBackend,
    ExecutionPolicy, ExportFamily, HybridSelectionMode, LibraryExecutionPolicy, LiveProtocolMode,
    ProfileConfig, ResolvedConfig, RunnerConfig, RunnerId, WebExecutionPolicy, WebGraphicsBridge,
    WebGraphicsCapability, WebRenderingModel, WebSurface, WebSurfaceRequirements,
    default_resolved_config, web_surface_requirements,
};
pub use project_manifest::{PackageKind, ProjectManifest};
pub use tests_config::{TestPhase, TestsConfig};
