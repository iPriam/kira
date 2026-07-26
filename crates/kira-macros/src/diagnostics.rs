//! The `KMAC` diagnostic family: every way a macro declaration or a macro call
//! site can be wrong.
//!
//! Codes are stable and are spelled here once. Expansion is total — it reports
//! and carries on, leaving the offending site unexpanded — so a file with a bad
//! macro still reaches semantics and still reports everything else wrong with
//! it.

use kira_diagnostics::{Diagnostic, Label, Severity};
use kira_source::{FileSpan, SourceId, Span};

/// Collects the diagnostics one expansion run produced.
#[derive(Debug, Default)]
pub(crate) struct Reporter {
    items: Vec<Diagnostic>,
}

impl Reporter {
    /// Creates an empty reporter.
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Records an error at `span` in `source`.
    pub(crate) fn error(
        &mut self,
        source: SourceId,
        span: Span,
        code: &'static str,
        message: impl Into<String>,
    ) {
        self.items.push(error(source, span, code, message));
    }

    /// Everything reported, in the order it was reported.
    pub(crate) fn into_diagnostics(self) -> Vec<Diagnostic> {
        self.items
    }
}

/// Builds one macro-expansion error.
pub(crate) fn error(
    source: SourceId,
    span: Span,
    code: &'static str,
    message: impl Into<String>,
) -> Diagnostic {
    let message = message.into();
    let file_span = FileSpan::new(source, span);
    let mut diagnostic = Diagnostic::single(
        Severity::Error,
        message.clone(),
        Label::primary(file_span, message),
    );
    diagnostic.code = Some(code);
    diagnostic.phase = Some("macro expansion");
    diagnostic
}

/// KMAC001 — a `name!(…)` call site names no macro.
pub(crate) const UNKNOWN_MACRO: &str = "KMAC001";
/// KMAC002 — a `name!(…)` call site passed the wrong number of fragments.
pub(crate) const ARGUMENT_COUNT: &str = "KMAC002";
/// KMAC003 — an `expr` fragment was given something that is not an expression.
pub(crate) const FRAGMENT_KIND: &str = "KMAC003";
/// KMAC004 — a `place` fragment was given a non-assignable argument.
pub(crate) const PLACE_NOT_ASSIGNABLE: &str = "KMAC004";
/// KMAC005 — a statement-only macro was used in expression position.
pub(crate) const STATEMENT_ONLY: &str = "KMAC005";
/// KMAC006 — a `comptime macro` has no `kind`, or names one that does not exist.
pub(crate) const BAD_KIND: &str = "KMAC006";
/// KMAC007 — a macro was applied to a declaration kind not in its `appliesTo`.
pub(crate) const APPLIES_TO: &str = "KMAC007";
/// KMAC008 — `appliesTo` is present on a `function` macro or missing elsewhere.
pub(crate) const APPLIES_TO_PRESENCE: &str = "KMAC008";
/// KMAC009 — a `#{ … }` splice of a value with no splice rule.
pub(crate) const NO_SPLICE_RULE: &str = "KMAC009";
/// KMAC010 — the expansion depth limit was exceeded.
pub(crate) const DEPTH_LIMIT: &str = "KMAC010";
/// KMAC011 — `@Derive(X)` where `X` is not a `derive`-kind macro.
pub(crate) const NOT_A_DERIVE: &str = "KMAC011";
/// KMAC012 — a `comptime macro`'s `expand` does not match its `kind`.
pub(crate) const EXPAND_SIGNATURE: &str = "KMAC012";
/// KMAC016 — a statement-position expansion that does not parse as statements.
pub(crate) const NOT_STATEMENTS: &str = "KMAC016";
/// KMAC017 — an expression-position expansion that is not a single expression.
pub(crate) const NOT_AN_EXPRESSION: &str = "KMAC017";
/// KMAC020 — an `expand` body used a construct the evaluator does not support.
pub(crate) const UNSUPPORTED_IN_EXPAND: &str = "KMAC020";
/// KMAC021 — a macro raised `Diagnostics.error`.
pub(crate) const MACRO_REPORTED: &str = "KMAC021";
/// KMAC022 — `ksl!` could not compile the shader it names.
pub(crate) const SHADER_COMPILE: &str = "KMAC022";
/// KMAC023 — `ksl!` was passed other than one argument.
pub(crate) const SHADER_ARGUMENT_COUNT: &str = "KMAC023";
/// KMAC024 — `ksl!` was passed a path that is not a string literal.
pub(crate) const SHADER_PATH_NOT_LITERAL: &str = "KMAC024";
/// KMAC025 — `Syntax.dropField` named a field the declaration does not have.
pub(crate) const NO_SUCH_FIELD: &str = "KMAC025";
/// KMAC026 — a declaration-only `Syntax` method on a non-declaration value.
pub(crate) const NOT_A_DECLARATION: &str = "KMAC026";
/// KMAC027 — an assignment through a wrapped property path.
pub(crate) const WRITE_THROUGH_WRAPPER: &str = "KMAC027";
/// KMAC028 — more than one replace-mode macro applied to one declaration.
pub(crate) const TWO_REPLACERS: &str = "KMAC028";
/// KMAC029 — a `trigger { field }` macro that is not `replace { true }`.
pub(crate) const TRIGGER_WITHOUT_REPLACE: &str = "KMAC029";
/// KMAC030 — two expansions claimed the same bytes (a bug in this crate).
pub(crate) const CONFLICTING_REWRITE: &str = "KMAC030";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reported_error_carries_its_code_and_phase() {
        let mut reporter = Reporter::new();
        reporter.error(
            SourceId::new(0),
            Span::new(0, 3),
            UNKNOWN_MACRO,
            "unknown macro `f`",
        );
        let items = reporter.into_diagnostics();
        assert_eq!(items[0].code, Some("KMAC001"));
        assert_eq!(items[0].phase, Some("macro expansion"));
    }
}
