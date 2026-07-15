//! Compiler phases a diagnostic can originate from.
//!
//! Mirrors kira-zig `packages/kira_diagnostic_messages/src/CompilerPhase.zig`.

/// The pipeline stage that produced a diagnostic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CompilerPhase {
    /// CLI argument parsing.
    CliArgumentParsing,
    /// Backend/target selection.
    TargetSelection,
    /// Project and manifest discovery.
    ProjectDiscovery,
    /// Lexing and parsing.
    Parser,
    /// Program graph construction.
    Graph,
    /// Semantic analysis.
    Semantics,
    /// HIR/IR lowering.
    Lowering,
    /// Backend preparation.
    BackendPrepare,
    /// Toolchain activation.
    ToolchainActivation,
    /// Runtime execution of the compiled program.
    RuntimeExecution,
    /// The crash-report boundary around the whole pipeline.
    CrashBoundary,
}

impl CompilerPhase {
    /// Returns the phase's tag as rendered in diagnostics (Zig `@tagName`).
    pub fn tag(self) -> &'static str {
        match self {
            CompilerPhase::CliArgumentParsing => "cli_argument_parsing",
            CompilerPhase::TargetSelection => "target_selection",
            CompilerPhase::ProjectDiscovery => "project_discovery",
            CompilerPhase::Parser => "parser",
            CompilerPhase::Graph => "graph",
            CompilerPhase::Semantics => "semantics",
            CompilerPhase::Lowering => "lowering",
            CompilerPhase::BackendPrepare => "backend_prepare",
            CompilerPhase::ToolchainActivation => "toolchain_activation",
            CompilerPhase::RuntimeExecution => "runtime_execution",
            CompilerPhase::CrashBoundary => "crash_boundary",
        }
    }
}
