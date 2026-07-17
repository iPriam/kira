//! Backend-neutral compile request/result API implemented by code-generation backends.
//!
//! Layer 3 of the Kira package graph. This is the seam every code-generation
//! backend (VM bytecode, LLVM/native, hybrid) implements. It is deliberately
//! minimal: the concrete program input type is supplied by [`Backend::compile`]
//! once the IR is designed.

/// Which artifact family a backend emits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BackendMode {
    /// VM bytecode for the interpreter.
    VmBytecode,
    /// Native code via the LLVM backend.
    LlvmNative,
    /// A hybrid bundle (bytecode plus native entry points).
    Hybrid,
}

impl BackendMode {
    /// This mode's spelling on the command line.
    ///
    /// The one place a mode becomes text, so a diagnostic naming a backend and
    /// the flag a user typed cannot drift apart.
    pub fn label(self) -> &'static str {
        match self {
            Self::VmBytecode => "vm",
            Self::LlvmNative => "llvm",
            Self::Hybrid => "hybrid",
        }
    }
}

/// Output paths for native/LLVM emission.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct NativeEmitOptions {
    /// Where the object file is written.
    pub object_path: String,
    /// Where the linked executable is written, if one is requested.
    pub executable_path: Option<String>,
    /// Where a shared library is written, if one is requested.
    pub shared_library_path: Option<String>,
    /// Where textual IR is written, if requested.
    pub ir_path: Option<String>,
}

/// A backend-neutral compile request.
#[derive(Debug, Clone)]
pub struct CompileRequest {
    /// The artifact family to emit.
    pub mode: BackendMode,
    /// The module name to record in emitted artifacts.
    pub module_name: String,
    /// Output paths for native/LLVM emission.
    pub emit: NativeEmitOptions,
}

/// Kind of an emitted artifact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ArtifactKind {
    /// VM bytecode.
    Bytecode,
    /// A native object file.
    NativeObject,
    /// A native shared/static library.
    NativeLibrary,
    /// A linked executable.
    Executable,
    /// A hybrid bundle.
    HybridBundle,
}

/// One emitted artifact.
#[derive(Debug, Clone, PartialEq)]
pub struct Artifact {
    /// What kind of artifact this is.
    pub kind: ArtifactKind,
    /// Where the artifact was written.
    pub path: String,
}

/// A compile result: the set of artifacts a backend emitted.
#[derive(Debug, Clone, Default)]
pub struct CompileResult {
    /// The emitted artifacts.
    pub artifacts: Vec<Artifact>,
}

/// A backend failure.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct BackendError(pub String);

/// A code-generation backend.
///
/// The verified program input is threaded through once the IR is designed;
/// until then the trait fixes the request/result contract every backend shares.
pub trait Backend {
    /// Compiles per `request`, returning the emitted artifacts.
    fn compile(&mut self, request: &CompileRequest) -> Result<CompileResult, BackendError>;
}
