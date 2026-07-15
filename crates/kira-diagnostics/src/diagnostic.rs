//! The diagnostic record itself: severity, code, labels, notes, help.

use crate::label::{Label, LabelKind};
use kira_source::FileSpan;

/// How serious a diagnostic is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    /// A hard error; compilation cannot succeed.
    Error,
    /// A warning; compilation continues.
    Warning,
    /// An informational note.
    Note,
}

/// A machine-applicable fix hint attached to a diagnostic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Suggestion {
    /// Human-readable description of the suggested fix.
    pub message: String,
}

/// One reported problem: severity, identity, message, and source anchors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    /// Error / warning / note.
    pub severity: Severity,
    /// Short one-line summary.
    pub title: String,
    /// Full explanatory message.
    pub message: String,
    /// Stable diagnostic code (e.g. `"KSEM032"`), when the message is cataloged.
    pub code: Option<&'static str>,
    /// Diagnostic domain tag (e.g. `"package"`), when cataloged.
    pub domain: Option<&'static str>,
    /// Compiler phase tag (e.g. `"parser"`), when cataloged.
    pub phase: Option<&'static str>,
    /// Span-anchored labels; the first primary label is the main site.
    pub labels: Vec<Label>,
    /// Free-form notes appended after the message.
    pub notes: Vec<String>,
    /// A "how to fix it" hint.
    pub help: Option<String>,
    /// A machine-applicable fix hint.
    pub suggestion: Option<Suggestion>,
}

impl Diagnostic {
    /// Builds a single-label diagnostic.
    pub fn single(severity: Severity, message: impl Into<String>, label: Label) -> Self {
        let message = message.into();
        Self {
            severity,
            title: message.clone(),
            message,
            code: None,
            domain: None,
            phase: None,
            labels: vec![label],
            notes: Vec::new(),
            help: None,
            suggestion: None,
        }
    }

    /// Returns the first primary label, falling back to the first label of any kind.
    pub fn primary_label(&self) -> Option<&Label> {
        self.labels
            .iter()
            .find(|label| label.kind == LabelKind::Primary)
            .or_else(|| self.labels.first())
    }

    /// Convenience accessor: the file span of the primary label, when present.
    pub fn primary_span(&self) -> Option<FileSpan> {
        self.primary_label().map(|label| label.span)
    }
}

/// True when any diagnostic in `items` is an error.
pub fn has_errors(items: &[Diagnostic]) -> bool {
    items
        .iter()
        .any(|diagnostic| diagnostic.severity == Severity::Error)
}
