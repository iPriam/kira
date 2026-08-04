//! The one shared builder every cataloged message goes through.

use crate::compiler_phase::CompilerPhase;
use crate::diagnostic_code::DiagnosticCode;
use crate::diagnostic_domain::DiagnosticDomain;
use kira_diagnostics::{Code, Diagnostic, Label, Severity};
use kira_source::FileSpan;

/// Everything a cataloged message provides to [`build`].
#[derive(Debug, Clone)]
pub struct MessageArgs {
    /// Stable diagnostic code.
    pub code: DiagnosticCode,
    /// Severity; catalog messages default to error.
    pub severity: Severity,
    /// Owning subsystem.
    pub domain: DiagnosticDomain,
    /// Originating pipeline stage, when known.
    pub phase: Option<CompilerPhase>,
    /// Short one-line summary.
    pub title: String,
    /// Full explanatory message.
    pub message: String,
    /// Main source location, when known.
    pub span: Option<FileSpan>,
    /// Label text for the span; defaults to `title`.
    pub label: Option<String>,
    /// Free-form notes appended after the message.
    pub notes: Vec<String>,
    /// A "how to fix it" hint.
    pub help: Option<String>,
}

/// Assembles a [`Diagnostic`] from catalog message arguments.
pub fn build(args: MessageArgs) -> Diagnostic {
    let labels = match args.span {
        Some(span) => vec![Label::primary(
            span,
            args.label.unwrap_or_else(|| args.title.clone()),
        )],
        None => Vec::new(),
    };
    Diagnostic {
        severity: args.severity,
        code: Some(Code::known(args.code.text())),
        domain: Some(args.domain.tag()),
        phase: args.phase.map(CompilerPhase::tag),
        title: args.title,
        message: args.message,
        labels,
        notes: args.notes,
        help: args.help,
        suggestion: None,
    }
}
