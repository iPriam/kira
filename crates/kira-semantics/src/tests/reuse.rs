//! What one compilation leaves behind for the next one, and what it must not.
//!
//! A file is parsed on its own and numbered from the base its position gives
//! it, so two compilations that hand over the same bytes at the same position
//! share the very same nodes. These tests pin both halves of that: the sharing
//! is real, and it stops exactly where the bytes stop matching — a program never
//! reads another program's file.

use super::*;
use salsa::Setter;

/// Builds a program from an entry file plus named modules.
fn program(db: &dyn salsa::Database, entry: &str, modules: &[(&str, &str)]) -> SourceProgram {
    let modules: Vec<ModuleSource> = modules
        .iter()
        .map(|&(module, text)| ModuleSource {
            module: module.to_owned(),
            path: format!("{module}.kira"),
            text: text.to_owned(),
        })
        .collect();
    SourceProgram::application(db, entry.to_owned(), "test.kira".to_owned(), modules)
}

const HELPER: (&str, &str) = ("helper", "function helper() -> Int { return 7 }");
const OTHER: (&str, &str) = ("other", "function other() -> Int { return 9 }");

/// Editing the entry file leaves every module's parse untouched — which is the
/// language server's whole workload, and the reason a file's handles are
/// numbered from its position rather than renumbered at assembly.
#[test]
fn editing_the_entry_file_reuses_every_modules_parse() {
    let mut db = salsa::DatabaseImpl::new();
    let source = program(
        &db,
        "import helper\nimport other\n@Main function main() { let v = helper() return }",
        &[HELPER, OTHER],
    );
    let first = parsed(&db, source).clone();

    // A longer entry, so the modules would be renumbered if assembly did any
    // renumbering at all.
    source.set_text(&mut db).to("import helper\nimport other\n\
         @Main function main() { let v = helper() let w = other() let x = v + w return }"
        .to_owned());
    let second = parsed(&db, source);

    assert_eq!(
        first.tree.shared_prefix(&second.tree),
        2,
        "both modules should be the same parse, not an equal one"
    );
}

/// Editing a module reuses the modules ahead of it and re-parses the rest.
///
/// The rest genuinely has to be re-parsed: a file's handles start where the
/// file before it ended, so a module that grew moves everything after it.
#[test]
fn editing_a_module_reuses_only_what_precedes_it() {
    let mut db = salsa::DatabaseImpl::new();
    let entry = "import helper\nimport other\n@Main function main() { let v = helper() return }";
    let source = program(&db, entry, &[HELPER, OTHER]);
    let first = parsed(&db, source).clone();

    let edited = ("other", "function other() -> Int { return 9 + 1 + 1 + 1 }");
    source.set_modules(&mut db).to(vec![
        ModuleSource {
            module: HELPER.0.to_owned(),
            path: "helper.kira".to_owned(),
            text: HELPER.1.to_owned(),
        },
        ModuleSource {
            module: edited.0.to_owned(),
            path: "other.kira".to_owned(),
            text: edited.1.to_owned(),
        },
    ]);
    let second = parsed(&db, source);

    assert_eq!(
        first.tree.shared_prefix(&second.tree),
        1,
        "the module ahead of the edit is reused and the edited one is not"
    );
}

/// A file reused from an earlier compilation carries no part of that
/// compilation into this one: what it declares is its own, and what the other
/// program declared is gone.
///
/// The isolation the reuse must not break. Two programs share a module and
/// differ in everything else, and neither can see the other's entry file.
#[test]
fn a_shared_module_carries_nothing_of_the_program_it_came_from() {
    let mut db = salsa::DatabaseImpl::new();
    let source = program(
        &db,
        "import helper\n@Main function main() { let v = helper() return }\n\
         function onlyInTheFirst() -> Int { return 1 }",
        &[HELPER],
    );
    let first: Vec<String> = analyzed(&db, source)
        .functions
        .iter()
        .map(|function| function.name.clone())
        .collect();
    assert!(first.iter().any(|name| name == "onlyInTheFirst"));

    source.set_text(&mut db).to(
        "import helper\n@Main function main() { let v = helper() return }\n\
             function onlyInTheSecond() -> Int { return 2 }"
            .to_owned(),
    );
    let second: Vec<String> = analyzed(&db, source)
        .functions
        .iter()
        .map(|function| function.name.clone())
        .collect();

    assert!(second.iter().any(|name| name == "onlyInTheSecond"));
    assert!(
        !second.iter().any(|name| name == "onlyInTheFirst"),
        "the previous call's declarations must be gone: {second:?}"
    );
    assert!(
        second.iter().any(|name| name == "helper"),
        "the shared module is still there: {second:?}"
    );
}

/// The same name written in two files is two symbols, and analysis must not
/// mistake either for the other.
///
/// A file interns its own names, so `Point` in one module and `Point` in
/// another are different handles for one spelling. Two packages each declaring
/// one is the case that would break first if anything compared symbols instead
/// of the text they stand for.
#[test]
fn one_spelling_in_two_files_is_two_declarations() {
    let diagnostics = module_diagnostics(
        "import first\nimport second\n@Main function main() { return }",
        &[
            ("first", "struct Point { var x: Int }"),
            ("second", "struct Point { var y: Int }"),
        ],
    );
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == Some("KSEM004")),
        "two declarations of one name in one program collide: {diagnostics:?}"
    );
}
