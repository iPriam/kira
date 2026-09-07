//! A macro is a name a file writes, so an import is what makes it nameable.
//!
//! Macros expand between lexing and parsing, before the analyzer's import
//! table exists, and merging every package's declarations into one environment
//! made a dependency's dependency's macros callable from a program that never
//! imported it — while an ordinary function from that same file was correctly
//! refused. These pin the two answers together.

use crate::{Diagnostic, ImportTable, ModuleSource, SourceProgram, analyzed};
use salsa::DatabaseImpl;

/// `Inner` declares a macro and a function that uses it.
const INNER: &str = "macro innerDouble(value: expr) {\n\
                     expand {\n\
                     value * 2\n\
                     }\n\
                     }\n\
                     function innerHelper() -> Int { return innerDouble!(3) }";

/// `Outer` imports `Inner` and nothing imports `Outer`'s dependencies for it.
const OUTER: &str = "import Inner\nfunction outerHelper() -> Int { return innerHelper() }";

fn program(modules: Vec<ModuleSource>, entry: &str) -> Vec<Diagnostic> {
    let db = DatabaseImpl::new();
    let source = SourceProgram::application(&db, entry.to_owned(), "main.kira".to_owned(), modules);
    analyzed::accumulated::<crate::DiagnosticAccumulator>(&db, source)
        .into_iter()
        .map(|accumulator| accumulator.0.clone())
        .collect()
}

fn two_packages() -> Vec<ModuleSource> {
    vec![
        ModuleSource {
            module: ImportTable::package_module_identity("Inner", "Inner"),
            path: "Inner/Inner.kira".to_owned(),
            text: INNER.to_owned(),
        },
        ModuleSource {
            module: ImportTable::package_module_identity("Outer", "Outer"),
            path: "Outer/Outer.kira".to_owned(),
            text: OUTER.to_owned(),
        },
    ]
}

/// Importing `Outer` does not lend you `Inner` — for a macro either.
#[test]
fn a_macro_from_a_package_the_file_never_imported_is_not_nameable() {
    let diagnostics = program(
        two_packages(),
        "import Outer\n@Main function main() { print(innerDouble!(21)) return }",
    );
    assert!(
        diagnostics.iter().any(|item| item.has_code("KMAC001")),
        "a transitive dependency's macro must not expand here: {diagnostics:?}"
    );
}

/// The same file, the same package, the same call — with the import written.
#[test]
fn a_macro_from_a_package_the_file_imports_is_nameable() {
    let diagnostics = program(
        two_packages(),
        "import Outer\nimport Inner\n@Main function main() { print(innerDouble!(21)) return }",
    );
    assert!(
        diagnostics.is_empty(),
        "an imported package's macro must expand: {diagnostics:?}"
    );
}

/// A package's own macro stays nameable inside the package that declares it,
/// which is what the filtering must not break.
#[test]
fn a_package_still_uses_the_macros_it_declares_itself() {
    let diagnostics = program(
        two_packages(),
        "import Outer\n@Main function main() { print(outerHelper()) return }",
    );
    assert!(
        diagnostics.is_empty(),
        "`Inner` uses its own macro and `Outer` imports `Inner`: {diagnostics:?}"
    );
}

/// The rule is the same one every other name obeys, and it is worth pinning
/// that the two answers agree rather than only that the macro one is refused.
#[test]
fn an_ordinary_name_from_that_same_file_is_refused_the_same_way() {
    let diagnostics = program(
        two_packages(),
        "import Outer\n@Main function main() { print(innerHelper()) return }",
    );
    assert!(
        diagnostics.iter().any(|item| item.has_code("KSEM061")),
        "a transitive dependency's function is not nameable either: {diagnostics:?}"
    );
}
