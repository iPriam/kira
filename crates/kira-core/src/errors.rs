//! Cross-cutting failure categories shared by every compiler stage.
//!
//! Mirrors kira-zig `packages/kira_core/src/errors.zig`.

/// Coarse error set every Kira pipeline stage can surface (Zig `CommonError`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum CommonError {
    /// The project manifest is missing or malformed.
    #[error("invalid manifest")]
    InvalidManifest,
    /// Lexing or parsing failed; diagnostics carry the details.
    #[error("parse failed")]
    ParseFailed,
    /// Semantic analysis failed; diagnostics carry the details.
    #[error("semantic analysis failed")]
    SemanticFailed,
    /// The program has no `@Main` entrypoint.
    #[error("missing main entrypoint")]
    MissingMain,
    /// The requested feature is not implemented yet.
    #[error("not implemented")]
    NotImplemented,
    /// The requested compilation target is not supported.
    #[error("unsupported target")]
    UnsupportedTarget,
    /// Execution of the compiled program failed at runtime.
    #[error("runtime failure")]
    RuntimeFailure,
}
