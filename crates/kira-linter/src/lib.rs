//! Shared lint policy and report accounting for Kira packages.
//!
//! Layer 8 of the Kira package graph. The executable lint rules are ordinary
//! Kira collector macros; this crate owns the policy around their diagnostics
//! so CLI and embedding callers agree about severity and summary counts.

use std::collections::BTreeMap;

use kira_diagnostics::{Diagnostic, Severity};

/// The severity a lint code should receive under a policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LintLevel {
    /// Do not report this lint.
    Allow,
    /// Report it without failing the command.
    Warn,
    /// Report it as an error.
    Deny,
}

impl LintLevel {
    /// Parses the spelling accepted by configuration adapters.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "allow" | "off" => Some(Self::Allow),
            "warn" | "warning" => Some(Self::Warn),
            "deny" | "error" => Some(Self::Deny),
            _ => None,
        }
    }

    fn severity(self) -> Option<Severity> {
        match self {
            Self::Allow => None,
            Self::Warn => Some(Severity::Warning),
            Self::Deny => Some(Severity::Error),
        }
    }
}

/// Policy for the `KLINT…` diagnostics emitted by a package's runner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LintPolicy {
    /// The level used when a code has no explicit override.
    pub default: LintLevel,
    overrides: BTreeMap<String, LintLevel>,
}

impl Default for LintPolicy {
    fn default() -> Self {
        Self {
            default: LintLevel::Warn,
            overrides: BTreeMap::new(),
        }
    }
}

impl LintPolicy {
    /// Creates a policy with `default` as its fallback.
    #[must_use]
    pub fn new(default: LintLevel) -> Self {
        Self {
            default,
            overrides: BTreeMap::new(),
        }
    }

    /// Sets or replaces one code-specific level.
    pub fn set(&mut self, code: impl Into<String>, level: LintLevel) {
        self.overrides.insert(code.into(), level);
    }

    /// Returns the configured level for `code`.
    #[must_use]
    pub fn level(&self, code: &str) -> LintLevel {
        self.overrides.get(code).copied().unwrap_or(self.default)
    }

    /// Applies the policy to lint diagnostics, leaving compiler diagnostics alone.
    #[must_use]
    pub fn apply(&self, diagnostics: &[Diagnostic]) -> Vec<Diagnostic> {
        diagnostics
            .iter()
            .filter_map(|diagnostic| {
                let code = diagnostic.code_text()?;
                if !code.starts_with("KLINT") {
                    return Some(diagnostic.clone());
                }
                let level = self.level(code);
                let severity = level.severity()?;
                let mut adjusted = diagnostic.clone();
                adjusted.severity = severity;
                Some(adjusted)
            })
            .collect()
    }
}

/// Counts the findings a lint run produced.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LintSummary {
    /// Error-severity findings.
    pub errors: usize,
    /// Warning-severity findings.
    pub warnings: usize,
    /// Informational findings.
    pub notes: usize,
    /// Findings carrying a machine-applicable suggestion.
    pub machine_applicable_fixes: usize,
}

/// Summarizes only actual lint findings; compiler diagnostics are ignored.
#[must_use]
pub fn summarize(diagnostics: &[Diagnostic]) -> LintSummary {
    diagnostics
        .iter()
        .filter(|diagnostic| is_lint(diagnostic))
        .fold(LintSummary::default(), |mut summary, diagnostic| {
            match diagnostic.severity {
                Severity::Error => summary.errors += 1,
                Severity::Warning => summary.warnings += 1,
                Severity::Note => summary.notes += 1,
            }
            if diagnostic
                .suggestion
                .as_ref()
                .is_some_and(|suggestion| suggestion.is_machine_applicable())
            {
                summary.machine_applicable_fixes += 1;
            }
            summary
        })
}

/// Whether a diagnostic belongs to the Kira lint namespace.
#[must_use]
pub fn is_lint(diagnostic: &Diagnostic) -> bool {
    diagnostic
        .code_text()
        .is_some_and(|code| code.starts_with("KLINT"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use kira_diagnostics::{Code, Label};
    use kira_source::{FileSpan, SourceId, Span};

    fn diagnostic(code: &'static str, severity: Severity) -> Diagnostic {
        let mut diagnostic = Diagnostic::single(
            severity,
            "finding",
            Label::primary(FileSpan::new(SourceId::new(0), Span::new(0, 1)), "here"),
        );
        diagnostic.code = Some(Code::known(code));
        diagnostic
    }

    #[test]
    fn policy_can_disable_or_promote_one_lint_without_touching_compiler_errors() {
        let mut policy = LintPolicy::new(LintLevel::Warn);
        policy.set("KLINT001", LintLevel::Allow);
        policy.set("KLINT002", LintLevel::Deny);
        let input = vec![
            diagnostic("KLINT001", Severity::Warning),
            diagnostic("KLINT002", Severity::Warning),
            diagnostic("KSEM001", Severity::Error),
        ];
        let output = policy.apply(&input);
        assert_eq!(output.len(), 2);
        assert_eq!(output[0].code_text(), Some("KLINT002"));
        assert_eq!(output[0].severity, Severity::Error);
        assert_eq!(output[1].code_text(), Some("KSEM001"));
    }

    #[test]
    fn summary_counts_only_lint_findings() {
        let summary = summarize(&[
            diagnostic("KLINT001", Severity::Warning),
            diagnostic("KLINT002", Severity::Error),
            diagnostic("KSEM001", Severity::Error),
        ]);
        assert_eq!(summary.warnings, 1);
        assert_eq!(summary.errors, 1);
    }
}
