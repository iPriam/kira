//! Diagnostic model: severities, labels, suggestions, and rendering.
//!
//! Layer 0 of the Kira package graph.

pub mod diagnostic;
pub mod label;
pub mod progress;
pub mod renderer;
pub mod sink;

pub use diagnostic::{Applicability, Code, Diagnostic, Severity, Suggestion, has_errors};
pub use label::{Label, LabelKind};
pub use sink::{DiagnosticSink, ErrorSpec};
