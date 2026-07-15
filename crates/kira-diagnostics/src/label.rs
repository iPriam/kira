//! Span-anchored labels attached to diagnostics.
//!
//! Mirrors kira-zig `packages/kira_diagnostics/src/label.zig`.

use kira_source::FileSpan;

/// Whether a label marks the main site of a diagnostic or extra context.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LabelKind {
    /// The main location the diagnostic points at.
    Primary,
    /// Additional context for the diagnostic.
    Secondary,
}

/// A message anchored to a span inside one source file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Label {
    /// Primary or secondary role of this label.
    pub kind: LabelKind,
    /// Where in the sources the label points.
    pub span: FileSpan,
    /// Text shown next to the underlined span.
    pub message: String,
}

impl Label {
    /// Builds a primary label (the Zig `primary` helper).
    pub fn primary(span: FileSpan, message: impl Into<String>) -> Self {
        Self {
            kind: LabelKind::Primary,
            span,
            message: message.into(),
        }
    }

    /// Builds a secondary label (the Zig `secondary` helper).
    pub fn secondary(span: FileSpan, message: impl Into<String>) -> Self {
        Self {
            kind: LabelKind::Secondary,
            span,
            message: message.into(),
        }
    }
}
