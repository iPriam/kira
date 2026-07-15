//! Diagnostic domains: which subsystem owns a diagnostic.

/// The subsystem a diagnostic belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DiagnosticDomain {
    /// Command-line interface.
    Cli,
    /// Package / project discovery and manifests.
    Package,
    /// Toolchain discovery and activation.
    Toolchain,
    /// Code-generation backends.
    Backend,
    /// HIR/IR lowering.
    Lowering,
    /// Internal compiler errors.
    CompilerInternal,
}

impl DiagnosticDomain {
    /// Returns the domain's tag as rendered in diagnostics.
    pub fn tag(self) -> &'static str {
        match self {
            DiagnosticDomain::Cli => "cli",
            DiagnosticDomain::Package => "package",
            DiagnosticDomain::Toolchain => "toolchain",
            DiagnosticDomain::Backend => "backend",
            DiagnosticDomain::Lowering => "lowering",
            DiagnosticDomain::CompilerInternal => "compiler_internal",
        }
    }
}
