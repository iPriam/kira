//! The `KSLP` diagnostic family: every way a KSL file can fail to parse.
//!
//! Separate from Kira's own `KPAR` family because the two languages are parsed
//! by different grammars and a shared code would say "the parser rejected it"
//! without saying which parser.

use kira_diagnostics::{Diagnostic, Label, Severity};
use kira_source::{FileSpan, SourceId, Span};

/// Collects what one parse reported.
#[derive(Debug)]
pub(crate) struct Reporter {
    source: SourceId,
    items: Vec<Diagnostic>,
}

impl Reporter {
    /// Creates a reporter for diagnostics in `source`.
    pub(crate) fn new(source: SourceId) -> Self {
        Self {
            source,
            items: Vec::new(),
        }
    }

    /// Records an error at `span`.
    pub(crate) fn error(&mut self, span: Span, code: &'static str, message: impl Into<String>) {
        let message = message.into();
        let file_span = FileSpan::new(self.source, span);
        let mut diagnostic = Diagnostic::single(
            Severity::Error,
            message.clone(),
            Label::primary(file_span, message),
        );
        diagnostic.code = Some(code);
        diagnostic.phase = Some("ksl parser");
        self.items.push(diagnostic);
    }

    /// Everything reported, in the order it was reported.
    pub(crate) fn into_diagnostics(self) -> Vec<Diagnostic> {
        self.items
    }
}

/// KSLP001 — a token appeared where the grammar allows something else.
pub(crate) const UNEXPECTED: &str = "KSLP001";
/// KSLP002 — a top-level position held something that is not a declaration.
pub(crate) const NOT_A_DECLARATION: &str = "KSLP002";
/// KSLP003 — a resource declaration is malformed.
pub(crate) const BAD_RESOURCE: &str = "KSLP003";
/// KSLP004 — a `storage` resource named an access mode that does not exist.
pub(crate) const BAD_ACCESS: &str = "KSLP004";
/// KSLP005 — an annotation is not one KSL defines, or is malformed.
pub(crate) const BAD_ANNOTATION: &str = "KSLP005";
/// KSLP006 — a stage body held something a stage cannot contain.
pub(crate) const BAD_STAGE_ITEM: &str = "KSLP006";
/// KSLP007 — `threads` was not given exactly three extents.
pub(crate) const BAD_THREADS: &str = "KSLP007";
/// KSLP008 — a numeric literal does not fit the type it is written as.
pub(crate) const LITERAL_RANGE: &str = "KSLP008";
/// KSLP009 — the file interned more distinct names than the compiler can hold.
pub(crate) const TOO_MANY_NAMES: &str = "KSLP009";
