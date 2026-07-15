//! Build artifacts.
//!
//! Ported from kira-zig `kira_build_definition/src/artifact.zig`.

/// Kind of a build artifact (Zig `ArtifactKind`).
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
    /// Zig `.hybrid_manifest`.
    HybridManifest,
    /// Zig `.documentation`.
    Documentation,
}

/// One build artifact (Zig `Artifact`).
#[derive(Debug, Clone, PartialEq)]
pub struct Artifact {
    /// Zig `kind: ArtifactKind`.
    pub kind: ArtifactKind,
    /// Zig `path: []const u8`.
    pub path: String,
}
