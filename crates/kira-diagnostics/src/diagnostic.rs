//! The diagnostic record itself: severity, code, labels, notes, help.

use std::borrow::Cow;
use std::fmt;

use crate::label::{Label, LabelKind};
use kira_source::FileSpan;

/// A diagnostic's stable identity: `KSEM032`, `KMAC021`, `KLINT014`.
///
/// Borrowed for a code this compiler catalogs, owned for one it does not. A
/// lint written in Kira names itself, and that name reaches here as a `String`
/// built at run time — there is no `&'static str` to be had for it, and leaking
/// one per lint per compile would grow without bound in a long-lived process.
/// So the type admits both and costs nothing for the cataloged case, which is
/// every code the compiler itself reports.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Code(Cow<'static, str>);

impl Code {
    /// A code this compiler catalogs.
    #[must_use]
    pub const fn known(code: &'static str) -> Self {
        Self(Cow::Borrowed(code))
    }

    /// A code named at run time, by a lint or anything else outside the
    /// catalog.
    #[must_use]
    pub fn named(code: impl Into<String>) -> Self {
        Self(Cow::Owned(code.into()))
    }

    /// The code as written.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Code {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<&'static str> for Code {
    fn from(code: &'static str) -> Self {
        Self::known(code)
    }
}

impl PartialEq<str> for Code {
    fn eq(&self, other: &str) -> bool {
        self.0 == other
    }
}

impl PartialEq<&str> for Code {
    fn eq(&self, other: &&str) -> bool {
        self.0 == *other
    }
}

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

/// How far a [`Suggestion`] can be trusted to be applied unattended.
///
/// The distinction is what separates a fix a tool may write for you from one it
/// may only show you. Applying a `MaybeIncorrect` automatically is how a
/// "helpful" tool silently changes what a program means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Applicability {
    /// The replacement preserves behaviour and can be applied unattended.
    MachineApplicable,
    /// The replacement is probably right, but a reader has to confirm it.
    MaybeIncorrect,
    /// The replacement contains placeholders a human must fill in.
    HasPlaceholders,
    /// Nothing is claimed about it, so nothing may apply it automatically.
    Unspecified,
}

/// A fix attached to a diagnostic: what to write, where, and how much to trust
/// it.
///
/// `message` is the phrase a reader sees after `= fix:`. The other three are
/// what a `--fix` pass needs, and they are carried even when nothing is
/// applying them yet — a suggestion that cannot say which bytes it replaces is
/// a sentence, not a fix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Suggestion {
    /// Human-readable description of the suggested fix.
    pub message: String,
    /// The bytes the replacement stands in for.
    pub span: FileSpan,
    /// What to write there. Empty removes the span.
    pub replacement: String,
    /// How far the replacement can be trusted.
    pub applicability: Applicability,
}

impl Suggestion {
    /// A fix that removes `span` outright, preserving behaviour.
    #[must_use]
    pub fn removal(message: impl Into<String>, span: FileSpan) -> Self {
        Self {
            message: message.into(),
            span,
            replacement: String::new(),
            applicability: Applicability::MachineApplicable,
        }
    }

    /// A fix that rewrites `span` to `replacement`, preserving behaviour.
    #[must_use]
    pub fn rewrite(
        message: impl Into<String>,
        span: FileSpan,
        replacement: impl Into<String>,
    ) -> Self {
        Self {
            message: message.into(),
            span,
            replacement: replacement.into(),
            applicability: Applicability::MachineApplicable,
        }
    }

    /// Whether a `--fix` pass may write this without asking.
    #[must_use]
    pub fn is_machine_applicable(&self) -> bool {
        self.applicability == Applicability::MachineApplicable
    }
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
    /// Stable diagnostic code (e.g. `"KSEM032"`), when the message has one.
    pub code: Option<Code>,
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

    /// The code as written, when it has one.
    #[must_use]
    pub fn code_text(&self) -> Option<&str> {
        self.code.as_ref().map(Code::as_str)
    }

    /// Whether this diagnostic is the one `code` names.
    ///
    /// Reads the same whether the code was cataloged or named at run time,
    /// which is the point: a caller asking "is this KLINT014?" should not have
    /// to know where the code came from.
    #[must_use]
    pub fn has_code(&self, code: &str) -> bool {
        self.code.as_ref().is_some_and(|written| written == code)
    }
}

/// True when any diagnostic in `items` is an error.
pub fn has_errors(items: &[Diagnostic]) -> bool {
    items
        .iter()
        .any(|diagnostic| diagnostic.severity == Severity::Error)
}
