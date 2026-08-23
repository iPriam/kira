//! Regression: a dependency package's enum must be nameable, both inside the
//! package that declares it and through an import — `FontWeight.Regular`
//! written in a theme package is the shape this guards.

use crate::{Diagnostic, ImportTable, ModuleSource, SourceProgram, analyzed};
use salsa::DatabaseImpl;

const LIB: &str = "enum Fruit { Apple Banana }\n\
                   function fruitName(fruit: Fruit) -> String {\n\
                   if fruit == .Apple { return \"a\" } return \"b\" }\n\
                   function defaultName() -> String { return fruitName(Fruit.Banana) }";

fn packaged(modules: Vec<ModuleSource>, entry: &str) -> Vec<Diagnostic> {
    let db = DatabaseImpl::new();
    let source =
        SourceProgram::application(&db, entry.to_owned(), "main.kira".to_owned(), modules);
    analyzed::accumulated::<crate::DiagnosticAccumulator>(&db, source)
        .into_iter()
        .map(|accumulator| accumulator.0.clone())
        .collect()
}

#[test]
fn dependency_package_internal_qualified_enum() {
    let modules = vec![
        ModuleSource {
            module: ImportTable::package_module_identity("Alpha", "Alpha"),
            path: "Alpha/Alpha.kira".to_owned(),
            text: String::new(),
        },
        ModuleSource {
            module: ImportTable::package_module_identity("Alpha", "Lib"),
            path: "Alpha/Lib.kira".to_owned(),
            text: LIB.to_owned(),
        },
    ];
    let diagnostics = packaged(
        modules,
        "import Alpha\n@Main function main() { print(defaultName()) return }",
    );
    assert!(
        diagnostics.is_empty(),
        "the library's own qualified enum use must resolve: {diagnostics:?}"
    );
}

#[test]
fn consumer_qualified_enum_through_import() {
    let modules = vec![ModuleSource {
        module: ImportTable::package_module_identity("Alpha", "Alpha"),
        path: "Alpha/Alpha.kira".to_owned(),
        text: LIB.to_owned(),
    }];
    let diagnostics = packaged(
        modules,
        "import Alpha\n@Main function main() { print(fruitName(Fruit.Banana)) return }",
    );
    assert!(
        diagnostics.is_empty(),
        "an imported package's enum is nameable qualified: {diagnostics:?}"
    );
}
