//! Analysis: one document's text in, this compiler's diagnostics out.
//!
//! The point of this module is that it contains no analysis. The language
//! server runs the *same* salsa frontend `kirac check` runs, so an editor
//! squiggle and a command-line error are the same computation and cannot drift
//! into two opinions about one program.
//!
//! # Why `analyzed` and not `lowered`
//!
//! The CLI collects diagnostics under its `lowered` query, which calls
//! `analyzed` and then lowers to IR. Lowering contributes no diagnostics of its
//! own — `kira-ir` does not even depend on salsa — so everything a user would
//! see from `kirac check` is already accumulated under `analyzed`. Reaching for
//! IR here would make the server depend on a backend crate to learn nothing.

use kira_diagnostics::Diagnostic;
use kira_semantics::{DiagnosticAccumulator, FILE_SOURCE_ID, SourceProgram};
use kira_source::{SourceFile, SourceId};

/// One analyzed document: its diagnostics, and the file they point into.
pub struct Analysis {
    /// Every diagnostic the frontend accumulated, in source order.
    pub diagnostics: Vec<Diagnostic>,
    /// The analyzed text, with the line table for mapping spans to positions.
    pub file: SourceFile,
}

/// The source id every single-file analysis uses.
///
/// Kira has no modules yet: one document is one program, and the frontend pins
/// it at this id. A diagnostic pointing anywhere else is one this server cannot
/// place, and it says so rather than guessing.
pub const DOCUMENT_SOURCE: SourceId = FILE_SOURCE_ID;

/// Analyzes one document's text.
///
/// A fresh database per call: salsa's incrementality is wasted here, because a
/// new `SourceProgram` input on each keystroke invalidates everything
/// downstream of it anyway. Reusing one database across edits is the
/// optimization this wants, and it is worth doing when a document is big enough
/// to notice — v0 programs are one file and analysis is microseconds.
pub fn analyze(path: &str, text: &str) -> Analysis {
    let db = salsa::DatabaseImpl::new();
    let source = SourceProgram::new(&db, text.to_owned(), path.to_owned());
    let _ = kira_semantics::analyzed(&db, source);
    let diagnostics = kira_semantics::analyzed::accumulated::<DiagnosticAccumulator>(&db, source)
        .into_iter()
        .map(|accumulated| accumulated.0.clone())
        .collect();

    Analysis {
        diagnostics,
        file: SourceFile::new(DOCUMENT_SOURCE, path.to_owned(), text.to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kira_diagnostics::Severity;

    /// New syntax reaches the editor for free, and this is what proves it: the
    /// server runs the compiler's own `analyzed` query, so a `for` loop it has
    /// never heard of squiggles exactly when `kirac check` says it should.
    #[test]
    fn loop_syntax_analyzes_the_way_the_compiler_does() {
        let clean = analyze(
            "t.kira",
            "@Main function main() { for i in 0..3 { if i > 1 { break } continue } return }",
        );
        assert!(clean.diagnostics.is_empty(), "{:?}", clean.diagnostics);

        let stray = analyze("t.kira", "@Main function main() { break return }");
        let diagnostic = stray
            .diagnostics
            .iter()
            .find(|diagnostic| diagnostic.code == Some("KSEM041"))
            .expect("a `break` outside a loop is reported");
        assert_eq!(diagnostic.severity, Severity::Error);
        assert!(!diagnostic.labels.is_empty(), "a span to squiggle");
    }

    #[test]
    fn a_clean_program_has_no_diagnostics() {
        let analysis = analyze("t.kira", "@Main function main() { print(1) return }");
        assert!(
            analysis.diagnostics.is_empty(),
            "{:?}",
            analysis.diagnostics,
        );
    }

    /// The server must surface what the compiler surfaces — this is the same
    /// code the CLI's `check` reports, reached through the same query.
    #[test]
    fn an_unknown_name_is_reported_with_its_code() {
        let analysis = analyze("t.kira", "@Main function main() { print(missing) return }");
        let diagnostic = analysis
            .diagnostics
            .iter()
            .find(|diagnostic| diagnostic.code == Some("KSEM060"))
            .expect("an unknown name is reported");
        assert_eq!(diagnostic.severity, Severity::Error);
        assert!(!diagnostic.labels.is_empty(), "a span to squiggle");
    }

    #[test]
    fn a_program_without_main_is_reported() {
        let analysis = analyze("t.kira", "function f() { return }");
        assert!(
            analysis
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == Some("KSEM011")),
            "{:?}",
            analysis.diagnostics,
        );
    }

    /// The parser is error-resilient, so a broken program still analyzes and
    /// still reports — an editor must not go blank on a half-typed line.
    #[test]
    fn a_syntactically_broken_program_still_reports_rather_than_bailing() {
        let analysis = analyze("t.kira", "@Main function main( {");
        assert!(!analysis.diagnostics.is_empty());
    }
}
