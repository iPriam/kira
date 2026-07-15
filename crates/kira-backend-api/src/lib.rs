//! Backend-neutral compile request/result API implemented by code-generation backends.
//!
//! Layer 3 of the Kira package graph.
//! Ported from kira-zig `packages/kira_backend_api` (66 LOC, ported fully).
//!
//! Port shape: the Zig `Backend` is a hand-rolled vtable
//! (`context: *anyopaque` + `compileFn`); the Rust port is the [`Backend`]
//! trait. The Zig `CompileRequest` embeds a `*const ir.VerifiedProgram`; to
//! keep the request type lifetime-free, the program is passed as a separate
//! borrowed argument to [`Backend::compile`].

use kira_ir::VerifiedProgram;
use kira_native_lib_definition::{AssetMount, ResolvedNativeLibrary, TargetSelector};

/// Which artifact family a backend emits (Zig `BackendMode`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BackendMode {
    /// Zig `.vm_bytecode`.
    VmBytecode,
    /// Zig `.llvm_native`.
    LlvmNative,
    /// Zig `.hybrid`.
    Hybrid,
}

/// Output paths for native/LLVM emission (Zig `NativeEmitOptions`).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct NativeEmitOptions {
    /// Zig `object_path: []const u8`.
    pub object_path: String,
    /// Zig `executable_path: ?[]const u8`.
    pub executable_path: Option<String>,
    /// Zig `shared_library_path: ?[]const u8`.
    pub shared_library_path: Option<String>,
    /// Zig `ir_path: ?[]const u8`.
    pub ir_path: Option<String>,
}

/// A compile request (Zig `CompileRequest`, minus the embedded program
/// pointer — see the crate docs).
#[derive(Debug, Clone)]
pub struct CompileRequest {
    /// Zig `mode: BackendMode`.
    pub mode: BackendMode,
    /// Zig `module_name: []const u8`.
    pub module_name: String,
    /// Zig `emit: NativeEmitOptions`.
    pub emit: NativeEmitOptions,
    /// Zig `target_selector: ?TargetSelector`.
    pub target_selector: Option<TargetSelector>,
    /// Zig `resolved_native_libraries: []const ResolvedNativeLibrary`.
    pub resolved_native_libraries: Vec<ResolvedNativeLibrary>,
    /// Zig `assets` — build-time asset directories to bundle into the linked
    /// executable; only the `wasm32-emscripten` link honours these.
    pub assets: Vec<AssetMount>,
}

/// Kind of an emitted artifact (Zig `ArtifactKind`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ArtifactKind {
    /// Zig `.bytecode`.
    Bytecode,
    /// Zig `.native_object`.
    NativeObject,
    /// Zig `.native_library`.
    NativeLibrary,
    /// Zig `.executable`.
    Executable,
    /// Zig `.hybrid_bundle`.
    HybridBundle,
}

/// One emitted artifact (Zig `Artifact`).
#[derive(Debug, Clone, PartialEq)]
pub struct Artifact {
    /// Zig `kind: ArtifactKind`.
    pub kind: ArtifactKind,
    /// Zig `path: []const u8`.
    pub path: String,
}

/// A compile result (Zig `CompileResult`).
#[derive(Debug, Clone, Default)]
pub struct CompileResult {
    /// Zig `artifacts: []const Artifact`.
    pub artifacts: Vec<Artifact>,
}

/// Backend failure (the Zig vtable's `anyerror`).
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct BackendError(pub String);

/// A code-generation backend (Zig `Backend`, a `context + compileFn` vtable).
///
/// Native/LLVM emission accepts only a verified-executable program: a backend
/// cannot be driven with a raw `ir::Program` — obtain a [`VerifiedProgram`]
/// from `ir::verify` (or, for trusted/test IR,
/// `VerifiedProgram::assume_verified`).
pub trait Backend {
    /// Compiles `program` per `request`, returning the emitted artifacts
    /// (Zig `Backend.compile`).
    fn compile(
        &mut self,
        program: &VerifiedProgram,
        request: &CompileRequest,
    ) -> Result<CompileResult, BackendError>;
}
