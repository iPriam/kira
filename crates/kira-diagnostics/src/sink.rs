//! Diagnostic collection: the sink every pipeline stage reports into.

use crate::diagnostic::{Code, Diagnostic, Severity, has_errors};
use crate::label::Label;
use kira_source::FileSpan;

/// Everything needed to emit one error diagnostic.
#[derive(Debug, Clone, Default)]
pub struct ErrorSpec {
    /// Stable diagnostic code, when the message has one.
    pub code: Option<Code>,
    /// Short one-line summary.
    pub title: String,
    /// Full explanatory message.
    pub message: String,
    /// Main source location, when known.
    pub span: Option<FileSpan>,
    /// Label text for the span; defaults to `title`.
    pub label: Option<String>,
    /// A "how to fix it" hint.
    pub help: Option<String>,
}

/// Ordered collector of diagnostics produced during a compilation stage.
#[derive(Debug, Default)]
pub struct DiagnosticSink {
    diagnostics: Vec<Diagnostic>,
}

impl DiagnosticSink {
    /// Creates an empty sink.
    pub fn new() -> Self {
        Self::default()
    }

    /// Appends an already-built diagnostic.
    pub fn push(&mut self, diagnostic: Diagnostic) {
        self.diagnostics.push(diagnostic);
    }

    /// Builds and appends an error diagnostic from a spec.
    pub fn error(&mut self, spec: ErrorSpec) {
        let labels = match spec.span {
            Some(span) => vec![Label::primary(
                span,
                spec.label.unwrap_or_else(|| spec.title.clone()),
            )],
            None => Vec::new(),
        };
        self.push(Diagnostic {
            severity: Severity::Error,
            title: spec.title,
            message: spec.message,
            code: spec.code,
            domain: None,
            phase: None,
            labels,
            notes: Vec::new(),
            help: spec.help,
            suggestion: None,
        });
    }

    /// True when at least one collected diagnostic is an error.
    pub fn has_errors(&self) -> bool {
        has_errors(&self.diagnostics)
    }

    /// Number of collected diagnostics.
    pub fn len(&self) -> usize {
        self.diagnostics.len()
    }

    /// True when nothing has been reported yet.
    pub fn is_empty(&self) -> bool {
        self.diagnostics.is_empty()
    }

    /// Read access to everything collected so far.
    pub fn as_slice(&self) -> &[Diagnostic] {
        &self.diagnostics
    }

    /// Consumes the sink, yielding the collected diagnostics.
    pub fn into_vec(self) -> Vec<Diagnostic> {
        self.diagnostics
    }
}
