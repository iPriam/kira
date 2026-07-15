//! Backend selection enums and the execution policy model.

use super::web::WebGraphicsBridge;

/// The three test/build backends of the parity matrix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    Vm,
    Llvm,
    Hybrid,
}

impl Backend {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "vm" => Some(Self::Vm),
            "llvm" => Some(Self::Llvm),
            "hybrid" => Some(Self::Hybrid),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Vm => "vm",
            Self::Llvm => "llvm",
            Self::Hybrid => "hybrid",
        }
    }
}

/// The execution backend an app or library runs on (superset of `Backend`
/// with the wasm execution modes).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionBackend {
    Vm,
    Llvm,
    Hybrid,
    WasmRuntime,
    WasmAot,
}

impl ExecutionBackend {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "vm" => Some(Self::Vm),
            "llvm" | "llvm_native" => Some(Self::Llvm),
            "hybrid" => Some(Self::Hybrid),
            "wasm_runtime" | "wasm-runtime" | "wasm" | "wasm32-emscripten" => {
                Some(Self::WasmRuntime)
            }
            "wasm_aot" | "wasm-aot" => Some(Self::WasmAot),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Vm => "vm",
            Self::Llvm => "llvm",
            Self::Hybrid => "hybrid",
            Self::WasmRuntime => "wasm_runtime",
            Self::WasmAot => "wasm_aot",
        }
    }
}

/// How functions are assigned to VM vs native execution in hybrid mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HybridSelectionMode {
    #[default]
    AnnotationDriven,
    NativeExceptRuntime,
    VmExceptNative,
    ExplicitOnly,
}

impl HybridSelectionMode {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "annotation_driven" | "annotation-driven" => Some(Self::AnnotationDriven),
            "native_except_runtime" | "native-except-runtime" => Some(Self::NativeExceptRuntime),
            "vm_except_native" | "vm-except-native" => Some(Self::VmExceptNative),
            "explicit_only" | "explicit-only" => Some(Self::ExplicitOnly),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::AnnotationDriven => "annotation_driven",
            Self::NativeExceptRuntime => "native_except_runtime",
            Self::VmExceptNative => "vm_except_native",
            Self::ExplicitOnly => "explicit_only",
        }
    }
}

/// Where a backend selection decision came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendSelectionSource {
    PlatformDefault,
    Profile,
    AppManifest,
    Cli,
}

impl BackendSelectionSource {
    pub fn label(self) -> &'static str {
        match self {
            Self::PlatformDefault => "platform-default",
            Self::Profile => "profile",
            Self::AppManifest => "app-manifest",
            Self::Cli => "cli",
        }
    }
}

/// Per-library execution backend override.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LibraryExecutionPolicy {
    pub package: String,
    pub backend: ExecutionBackend,
    pub source: BackendSelectionSource,
    pub native_required: bool,
    pub ffi_allowed: bool,
    pub hybrid_selection: Option<HybridSelectionMode>,
}

impl LibraryExecutionPolicy {
    /// A policy with the standard field defaults.
    pub fn new(package: impl Into<String>) -> Self {
        Self {
            package: package.into(),
            backend: ExecutionBackend::Hybrid,
            source: BackendSelectionSource::AppManifest,
            native_required: false,
            ffi_allowed: false,
            hybrid_selection: None,
        }
    }
}

/// Web-target execution policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WebExecutionPolicy {
    pub backend: ExecutionBackend,
    pub graphics_bridge: WebGraphicsBridge,
}

impl Default for WebExecutionPolicy {
    fn default() -> Self {
        Self {
            backend: ExecutionBackend::WasmRuntime,
            graphics_bridge: WebGraphicsBridge::None,
        }
    }
}

/// The resolved execution policy of a project.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionPolicy {
    pub default_backend: ExecutionBackend,
    pub default_source: BackendSelectionSource,
    pub hybrid_selection: HybridSelectionMode,
    pub libraries: Vec<LibraryExecutionPolicy>,
    pub web: WebExecutionPolicy,
}

impl Default for ExecutionPolicy {
    fn default() -> Self {
        Self {
            default_backend: ExecutionBackend::Vm,
            default_source: BackendSelectionSource::PlatformDefault,
            hybrid_selection: HybridSelectionMode::AnnotationDriven,
            libraries: Vec::new(),
            web: WebExecutionPolicy::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn execution_backend_policy_uses_typed_internal_values() {
        assert_eq!(
            Some(ExecutionBackend::WasmRuntime),
            ExecutionBackend::parse("wasm_runtime")
        );
        assert_eq!(
            Some(ExecutionBackend::WasmRuntime),
            ExecutionBackend::parse("wasm32-emscripten")
        );
        assert_eq!(
            Some(ExecutionBackend::WasmAot),
            ExecutionBackend::parse("wasm-aot")
        );
        assert_eq!(
            Some(HybridSelectionMode::NativeExceptRuntime),
            HybridSelectionMode::parse("native_except_runtime")
        );
        assert_eq!("app-manifest", BackendSelectionSource::AppManifest.label());
    }
}
