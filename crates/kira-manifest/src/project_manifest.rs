//! Root project manifest model (the `Package` declaration in `package.kira`).

use kira_native_lib_definition::NativeLibrarySpec;

use crate::dependency::{DependencyMutationError, DependencySpec};
use crate::platform_config::{ExecutionPolicy, ResolvedConfig, default_resolved_config};
use crate::tests_config::TestsConfig;

/// Whether a package builds an application or a library.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackageKind {
    App,
    Library,
}

impl PackageKind {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "app" => Some(Self::App),
            "library" => Some(Self::Library),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::App => "app",
            Self::Library => "library",
        }
    }
}

/// The root manifest model loaded from a `package.kira` declaration (or a
/// legacy `kira.toml`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectManifest {
    pub name: String,
    pub version: String,
    pub kind: PackageKind,
    pub kira_version: String,
    pub module_root: Option<String>,
    /// The C libraries declared inline as `let nativeLibraries = [...]`.
    ///
    /// A package may instead ship a `NativeLibs/<name>.toml` per library; both
    /// spellings decode into the same [`NativeLibrarySpec`], and a build reads
    /// both sources.
    pub native_libraries: Vec<NativeLibrarySpec>,
    /// Allows this package's static native archives to be wrapped in a thin
    /// symbol-carrier shared library for libffi calls.
    pub allow_thin_ffi_shim: bool,
    /// Project-root-relative directories (or files) bundled into a
    /// self-contained `wasm32-emscripten` package via emcc `--preload-file`.
    /// Accepted (and validated) on every target; only wasm builds package
    /// them, because host/native builds read the same paths from disk at
    /// runtime.
    pub assets: Vec<String>,
    pub dependencies: Vec<DependencySpec>,
    pub packages: Vec<String>,
    pub execution_mode: String,
    pub execution_policy: ExecutionPolicy,
    pub build_target: String,
    pub registry_url: Option<String>,
    pub registry_token_env: Option<String>,
    /// The `Tests { backends: [...], phase: ... }` config honored by
    /// `kira test`. `None` means the manifest omitted it (runner keeps
    /// historical behavior).
    pub tests: Option<TestsConfig>,
    pub resolved_config: ResolvedConfig,
}

impl ProjectManifest {
    /// A manifest with the standard field defaults.
    pub fn new(name: impl Into<String>, version: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            version: version.into(),
            kind: PackageKind::App,
            kira_version: "0.1.0".to_string(),
            module_root: None,
            native_libraries: Vec::new(),
            allow_thin_ffi_shim: false,
            assets: Vec::new(),
            dependencies: Vec::new(),
            packages: Vec::new(),
            execution_mode: "vm".to_string(),
            execution_policy: ExecutionPolicy::default(),
            build_target: "host".to_string(),
            registry_url: None,
            registry_token_env: None,
            tests: None,
            resolved_config: default_resolved_config(),
        }
    }

    /// Adds one dependency while preserving declaration order.
    pub fn add_dependency(
        &mut self,
        dependency: DependencySpec,
    ) -> Result<(), DependencyMutationError> {
        if self
            .dependencies
            .iter()
            .any(|existing| existing.name == dependency.name)
        {
            return Err(DependencyMutationError::Duplicate(dependency.name));
        }
        self.dependencies.push(dependency);
        Ok(())
    }

    /// Removes a dependency by name, returning the removed declaration.
    pub fn remove_dependency(&mut self, name: &str) -> Option<DependencySpec> {
        let index = self
            .dependencies
            .iter()
            .position(|dependency| dependency.name == name)?;
        Some(self.dependencies.remove(index))
    }
}
