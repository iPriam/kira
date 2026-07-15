//! Stable diagnostic codes (`KIC*`, `KPK*`, `KCL*`, `KTC*`, `KBE*`, ...).
//!
//! Mirrors kira-zig `packages/kira_diagnostic_messages/src/DiagnosticCode.zig`.
//!
//! TODO(port): the Zig catalog carries ~70 codes across the KIC / KIR / KBE /
//! KTC / KPK / KCL families (plus KSEM codes owned by semantics). Only a few
//! representative entries are scaffolded here; the full catalog ports
//! mechanically during migration, keeping the `text()` mapping exhaustive.

/// One stable, user-facing diagnostic code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DiagnosticCode {
    /// KIC001 — generic internal compiler error (approved fallback only).
    Kic001GenericInternalCompilerError,
    /// KBE001 — a backend was asked for a feature it does not support.
    Kbe001UnsupportedBackendFeature,
    /// KTC001 — the LLVM toolchain could not be found.
    Ktc001MissingLlvmToolchain,
    /// KPK001 — no project manifest was found.
    Kpk001MissingProjectManifest,
    /// KPK007 — the target entrypoint source file is missing.
    Kpk007MissingSourceFile,
    /// KPK010 — a library target has no buildable source files.
    Kpk010NoBuildableTarget,
    /// KCL001 — unknown CLI command.
    Kcl001UnknownCommand,
}

impl DiagnosticCode {
    /// Returns the code's user-facing text (e.g. `"KPK001"`).
    pub fn text(self) -> &'static str {
        match self {
            DiagnosticCode::Kic001GenericInternalCompilerError => "KIC001",
            DiagnosticCode::Kbe001UnsupportedBackendFeature => "KBE001",
            DiagnosticCode::Ktc001MissingLlvmToolchain => "KTC001",
            DiagnosticCode::Kpk001MissingProjectManifest => "KPK001",
            DiagnosticCode::Kpk007MissingSourceFile => "KPK007",
            DiagnosticCode::Kpk010NoBuildableTarget => "KPK010",
            DiagnosticCode::Kcl001UnknownCommand => "KCL001",
        }
    }
}
