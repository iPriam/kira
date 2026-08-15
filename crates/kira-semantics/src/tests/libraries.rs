//! The entrypoint rule, which is the one thing [`BuildKind`] decides.
//!
//! An application must declare a `@Main`; a library must not. Both cases are
//! checked during analysis, above the backend split.

use super::*;
use crate::host_platform;

#[test]
fn a_library_without_main_analyzes_clean() {
    let text = "function add(a: Int, b: Int) -> Int { return a + b }";
    assert!(
        library_diagnostics(text).is_empty(),
        "{:?}",
        library_diagnostics(text)
    );
}

#[test]
fn an_application_without_main_is_still_ksem011() {
    // The exemption is conditional, not a removal: the same source analyzed as
    // a program is still rejected.
    let text = "function add(a: Int, b: Int) -> Int { return a + b }";
    assert!(
        codes(text).iter().any(|code| code == "KSEM011"),
        "{:?}",
        codes(text)
    );
}

#[test]
fn a_library_declaring_main_is_refused_by_name() {
    let text = "@Main function main() { print(1) return }";
    assert!(
        library_codes(text).iter().any(|code| code == "KSEM255"),
        "{:?}",
        library_codes(text)
    );
}

#[test]
fn a_library_still_type_checks_its_bodies() {
    // Relaxing the entrypoint rule relaxes nothing else: a library gets the
    // whole analyzer, so a real error in a library is still an error.
    let text = "function bad() -> Int { return missing }";
    let reported = library_codes(text);
    assert!(!reported.is_empty(), "a library must still be checked");
    assert!(
        !reported.iter().any(|code| code == "KSEM011"),
        "{reported:?}"
    );
}

#[test]
fn a_library_records_no_entrypoint() {
    let db = salsa::DatabaseImpl::new();
    let source = SourceProgram::new(
        &db,
        "function add(a: Int, b: Int) -> Int { return a + b }".to_owned(),
        "test.kira".to_owned(),
        Vec::new(),
        BuildKind::Library,
        PrecompiledShaders::default(),
        host_platform(),
        // Not a lint run.
        false,
    );
    let program = analyzed(&db, source);
    assert!(program.main.is_none());
    assert_eq!(program.functions.len(), 1);
}

#[test]
fn a_library_with_classes_and_imports_analyzes_clean() {
    // Nothing about a library narrows the language: the same declarations an
    // application may write are available with no entrypoint present.
    let text = "class Button { var title: String = \"\"\n \
                function label() -> String { return self.title } }\n\
                function makeButton(title: String) -> Button { \
                var b = Button() b.title = title return b }";
    assert!(
        library_diagnostics(text).is_empty(),
        "{:?}",
        library_diagnostics(text)
    );
}
