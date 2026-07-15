//! Diagnostic model: severities, labels, suggestions, and rendering.
//!
//! Layer 0 of the Kira package graph.
//! Ported from kira-zig `packages/kira_diagnostics`.

pub mod diagnostic;
pub mod label;
pub mod renderer;
pub mod sink;

pub use diagnostic::{Diagnostic, Severity, Suggestion, has_errors};
pub use label::{Label, LabelKind};
pub use sink::{DiagnosticSink, ErrorSpec};
