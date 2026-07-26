//! The `KSLS` diagnostic family: every way a KSL file can parse and still be
//! wrong.

use kira_diagnostics::{Diagnostic, Label, Severity};
use kira_source::{FileSpan, SourceId, Span};

/// Collects what one check reported.
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

    /// Points later diagnostics at `source`, which is what an imported file
    /// needs so its errors are shown against their own text.
    pub(crate) fn switch_to(&mut self, source: SourceId) {
        self.source = source;
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
        diagnostic.phase = Some("ksl semantics");
        self.items.push(diagnostic);
    }

    /// Everything reported, in the order it was reported.
    pub(crate) fn into_diagnostics(self) -> Vec<Diagnostic> {
        self.items
    }
}

/// KSLS001 — a written type names nothing.
pub(crate) const UNKNOWN_TYPE: &str = "KSLS001";
/// KSLS002 — a name is not bound here.
pub(crate) const UNKNOWN_NAME: &str = "KSLS002";
/// KSLS003 — two declarations claim the same name.
pub(crate) const DUPLICATE: &str = "KSLS003";
/// KSLS004 — an expression has a type the position does not accept.
pub(crate) const TYPE_MISMATCH: &str = "KSLS004";
/// KSLS005 — a call was given the wrong number of arguments.
pub(crate) const ARGUMENT_COUNT: &str = "KSLS005";
/// KSLS006 — a member does not exist on the type it was read from.
pub(crate) const NO_SUCH_MEMBER: &str = "KSLS006";
/// KSLS007 — a `@builtin` names something, or sits somewhere, that is illegal.
pub(crate) const BAD_BUILTIN: &str = "KSLS007";
/// KSLS008 — a write to something that cannot be written.
pub(crate) const NOT_ASSIGNABLE: &str = "KSLS008";
/// KSLS009 — a stage is missing something it needs, or has one thing twice.
pub(crate) const BAD_STAGE: &str = "KSLS009";
/// KSLS010 — an option's default is not a constant of its type.
pub(crate) const BAD_OPTION: &str = "KSLS010";
/// KSLS011 — an import names a module that could not be loaded.
pub(crate) const UNRESOLVED_IMPORT: &str = "KSLS011";
/// KSLS012 — a resource declaration is illegal for the kind it declares.
pub(crate) const BAD_RESOURCE: &str = "KSLS012";
/// KSLS013 — a file declares no shader, or more than one.
pub(crate) const SHADER_COUNT: &str = "KSLS013";
/// KSLS014 — an operator was applied to types it does not accept.
pub(crate) const BAD_OPERATOR: &str = "KSLS014";
/// KSLS015 — a function can finish without returning its result.
pub(crate) const MISSING_RETURN: &str = "KSLS015";
