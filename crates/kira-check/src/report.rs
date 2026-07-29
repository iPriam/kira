//! Turning frontend diagnostics into the values a Kira caller reads.
//!
//! The one lossy step in the whole surface, and it is lossy on purpose: a
//! caller asserting on a compile failure must compare a *code* and a *file*,
//! never a message. Messages get reworded; a code does not, and a file is what
//! tells a two-file test which of its files the compiler objected to.

use kira_diagnostics::{Diagnostic, Severity};
use kira_runtime_abi::{CheckDiagnostic, CheckSeverity};
use kira_semantics::{FILE_SOURCE_ID, ModuleSource, module_source_id};
use kira_source::SourceId;

/// Flattens frontend diagnostics into the seam's value form.
///
/// `entry` is the path the entry file is known by, and `modules` are the rest
/// in source-id order — the same order the frontend assigned them — so a span
/// resolves back to the file it was written in.
#[must_use]
pub fn flatten(
    diagnostics: &[Diagnostic],
    modules: &[ModuleSource],
    entry: Option<&str>,
) -> Vec<CheckDiagnostic> {
    diagnostics
        .iter()
        .map(|diagnostic| CheckDiagnostic {
            code: diagnostic.code.unwrap_or_default().to_owned(),
            severity: severity_of(diagnostic.severity),
            file: file_of(diagnostic, modules, entry),
            title: diagnostic.title.clone(),
            message: diagnostic.message.clone(),
        })
        .collect()
}

/// The seam's spelling of one diagnostic severity.
fn severity_of(severity: Severity) -> CheckSeverity {
    match severity {
        Severity::Error => CheckSeverity::Error,
        Severity::Warning => CheckSeverity::Warning,
        Severity::Note => CheckSeverity::Note,
    }
}

/// The path of the file a diagnostic points into, empty when it points at none.
///
/// A package-level diagnostic — an unreadable manifest, a root that names no
/// package — has no span, and answers with the empty string rather than with
/// some file that happens to be first.
fn file_of(diagnostic: &Diagnostic, modules: &[ModuleSource], entry: Option<&str>) -> String {
    let Some(span) = diagnostic.primary_span() else {
        return String::new();
    };
    path_of(span.source, modules, entry).unwrap_or_default()
}

/// The path a source id names, or `None` when it names no file of this program.
fn path_of(source: SourceId, modules: &[ModuleSource], entry: Option<&str>) -> Option<String> {
    if source == FILE_SOURCE_ID {
        return entry.map(str::to_owned);
    }
    modules
        .iter()
        .enumerate()
        .find(|&(index, _)| module_source_id(index) == source)
        .map(|(_, module)| module.path.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use kira_diagnostics::Label;
    use kira_source::{FileSpan, Span};

    fn module(path: &str) -> ModuleSource {
        ModuleSource {
            module: path.to_owned(),
            path: path.to_owned(),
            text: String::new(),
        }
    }

    fn at(source: SourceId) -> Diagnostic {
        let mut diagnostic = Diagnostic::single(
            Severity::Error,
            "a problem",
            Label::primary(
                FileSpan {
                    source,
                    span: Span::new(0, 1),
                },
                "here",
            ),
        );
        diagnostic.code = Some("KSEM061");
        diagnostic
    }

    #[test]
    fn a_diagnostic_names_the_file_its_span_is_in() {
        let modules = [module("app/A.kira"), module("app/B.kira")];
        let flat = flatten(
            &[at(FILE_SOURCE_ID), at(module_source_id(1))],
            &modules,
            Some("app/main.kira"),
        );
        assert_eq!(flat[0].file, "app/main.kira");
        assert_eq!(flat[0].code, "KSEM061");
        assert_eq!(flat[0].severity, CheckSeverity::Error);
        assert_eq!(flat[1].file, "app/B.kira");
    }

    #[test]
    fn a_spanless_diagnostic_names_no_file() {
        let diagnostic = kira_diagnostic_messages::package_messages::unknown_root_package("App");
        let flat = flatten(&[diagnostic], &[], None);
        assert_eq!(flat[0].file, "");
        assert_eq!(flat[0].code, "KPK031");
    }

    #[test]
    fn every_severity_crosses_as_itself() {
        for (severity, expected) in [
            (Severity::Error, CheckSeverity::Error),
            (Severity::Warning, CheckSeverity::Warning),
            (Severity::Note, CheckSeverity::Note),
        ] {
            assert_eq!(severity_of(severity), expected);
        }
    }
}
