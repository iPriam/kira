use super::*;

/// A package's declarations are nameable only where the package is imported.
///
/// This is the whole of the visibility rule: one program's own files share a
/// flat scope, a dependency's names arrive through an import, and neither
/// reaches further than that.
#[test]
fn a_package_declaration_needs_an_import_to_be_named() {
    let db = salsa::DatabaseImpl::new();
    let modules = vec![ModuleSource {
        module: ImportTable::package_module_identity("Widgets", "Widgets"),
        path: "Widgets/Widgets.kira".to_owned(),
        text: "struct Panel { var w: Int }\nfunction makePanel() -> Int { return 1 }".to_owned(),
    }];
    let importing = SourceProgram::application(
        &db,
        "import Widgets\n@Main function main() { print(makePanel()) return }".to_owned(),
        "main.kira".to_owned(),
        modules.clone(),
    );
    assert!(
        analyzed::accumulated::<DiagnosticAccumulator>(&db, importing).is_empty(),
        "an imported package's function is nameable"
    );

    let without = SourceProgram::application(
        &db,
        "@Main function main() { print(makePanel()) return }".to_owned(),
        "main.kira".to_owned(),
        modules,
    );
    let diagnostics = analyzed::accumulated::<DiagnosticAccumulator>(&db, without);
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.0.has_code("KSEM061")),
        "without the import the name is not in scope: {diagnostics:?}"
    );
}

/// What a package imports does not become its consumer's vocabulary.
#[test]
fn visibility_does_not_compose_through_a_dependencys_own_imports() {
    let db = salsa::DatabaseImpl::new();
    let modules = vec![
        ModuleSource {
            module: ImportTable::package_module_identity("Inner", "Inner"),
            path: "Inner/Inner.kira".to_owned(),
            text: "function innerOnly() -> Int { return 1 }".to_owned(),
        },
        ModuleSource {
            module: ImportTable::package_module_identity("Outer", "Outer"),
            path: "Outer/Outer.kira".to_owned(),
            text: "import Inner\nfunction outerCalls() -> Int { return innerOnly() }".to_owned(),
        },
    ];
    let source = SourceProgram::application(
        &db,
        "import Outer\n@Main function main() { print(innerOnly()) return }".to_owned(),
        "main.kira".to_owned(),
        modules,
    );
    let diagnostics = analyzed::accumulated::<DiagnosticAccumulator>(&db, source);
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.0.has_code("KSEM061")),
        "`Outer` importing `Inner` does not lend `Inner` to whoever imports `Outer`: \
         {diagnostics:?}"
    );
}

/// Two packages may each declare the same name, and each means its own.
///
/// The name index is keyed by owner, so neither declaration is a duplicate of
/// the other; a file that imports both is what would have to disambiguate, and
/// a file that imports one simply gets that one.
#[test]
fn two_packages_may_declare_the_same_struct_name() {
    let db = salsa::DatabaseImpl::new();
    let modules = vec![
        ModuleSource {
            module: ImportTable::package_module_identity("First", "First"),
            path: "First/First.kira".to_owned(),
            text: "struct Handle { var id: Int }\n\
                   function firstHandle() -> Int { return Handle { id: 1 }.id }"
                .to_owned(),
        },
        ModuleSource {
            module: ImportTable::package_module_identity("Second", "Second"),
            path: "Second/Second.kira".to_owned(),
            text: "struct Handle { var tag: Int\nvar extra: Int }\n\
                   function secondHandle() -> Int { return Handle { tag: 2, extra: 3 }.extra }"
                .to_owned(),
        },
    ];
    let source = SourceProgram::application(
        &db,
        "import First\nimport Second\n\
         @Main function main() { print(firstHandle()) print(secondHandle()) return }"
            .to_owned(),
        "main.kira".to_owned(),
        modules,
    );
    let diagnostics = analyzed::accumulated::<DiagnosticAccumulator>(&db, source);
    assert!(
        diagnostics.is_empty(),
        "each package's `Handle` is its own declaration, with its own fields: {diagnostics:?}"
    );
}

/// Declaring the same name twice *inside* one package is still a duplicate.
#[test]
fn one_package_may_not_declare_the_same_struct_name_twice() {
    let db = salsa::DatabaseImpl::new();
    let modules = vec![
        ModuleSource {
            module: ImportTable::package_module_identity("Only", "Only"),
            path: "Only/Only.kira".to_owned(),
            text: "struct Handle { var id: Int }".to_owned(),
        },
        ModuleSource {
            module: ImportTable::package_module_identity("Only", "Again"),
            path: "Only/Again.kira".to_owned(),
            text: "struct Handle { var other: Int }".to_owned(),
        },
    ];
    let source = SourceProgram::application(
        &db,
        "import Only\n@Main function main() { return }".to_owned(),
        "main.kira".to_owned(),
        modules,
    );
    let diagnostics = analyzed::accumulated::<DiagnosticAccumulator>(&db, source);
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.0.has_code("KSEM004")),
        "one package is one flat scope, so the second declaration collides: {diagnostics:?}"
    );
}

/// A same-named declaration in another package does not capture a bare name.
///
/// The shape that sent the corpus wrong: one package declares a widget `Text`
/// and another declares its own `Text` function. A file of the second package
/// means its own, and never has to know the first exists.
#[test]
fn a_local_declaration_wins_over_a_same_named_one_in_another_package() {
    let db = salsa::DatabaseImpl::new();
    let modules = vec![
        ModuleSource {
            module: ImportTable::package_module_identity("Widgets", "Widgets"),
            path: "Widgets/Widgets.kira".to_owned(),
            text: "struct Text { var content: Int }".to_owned(),
        },
        ModuleSource {
            module: ImportTable::package_module_identity("Views", "Views"),
            path: "Views/Views.kira".to_owned(),
            text: "function Text(a: Int, b: Int) -> Int { return a + b }\n\
                   function useText() -> Int { return Text(1, 2) }"
                .to_owned(),
        },
    ];
    let source = SourceProgram::application(
        &db,
        "import Views\n@Main function main() { print(useText()) return }".to_owned(),
        "main.kira".to_owned(),
        modules,
    );
    let diagnostics = analyzed::accumulated::<DiagnosticAccumulator>(&db, source);
    assert!(
        diagnostics.is_empty(),
        "`Views` means its own `Text`, not the struct in `Widgets`: {diagnostics:?}"
    );
}

/// The package that declares `Color`, the package that speaks it, and a
/// consumer that declares a `Color` of its own — the corpus shape behind the
/// qualified-name rule.
fn palette_modules() -> Vec<ModuleSource> {
    vec![
        ModuleSource {
            module: ImportTable::package_module_identity("Palette", "Palette"),
            path: "Palette/Palette.kira".to_owned(),
            text: "struct Color { var rgba: Int }".to_owned(),
        },
        ModuleSource {
            module: ImportTable::package_module_identity("Views", "Views"),
            path: "Views/Views.kira".to_owned(),
            text: "import Palette\nstruct View { var fill: Color }".to_owned(),
        },
    ]
}

/// A package may not declare a type whose name an import already provides.
///
/// `Widgets` imports `Palette`, which declares `Color`, so a `Color` of its own
/// would shadow that one — refused. One namespace means one declaration per
/// name; the escape is to speak `Palette.Color` or to choose a different name,
/// never to keep two `Color`s that read as `expects Color, found Color`.
#[test]
fn a_local_type_may_not_shadow_an_imported_one() {
    let db = salsa::DatabaseImpl::new();
    let mut modules = palette_modules();
    modules.push(ModuleSource {
        module: ImportTable::package_module_identity("Widgets", "Widgets"),
        path: "Widgets/Widgets.kira".to_owned(),
        text: "import Palette\n\
               struct Color { var name: Int }\n\
               function ownColor() -> Int { return Color { name: 2 }.name }"
            .to_owned(),
    });
    let source = SourceProgram::application(
        &db,
        "import Widgets\n@Main function main() { print(ownColor()) return }".to_owned(),
        "main.kira".to_owned(),
        modules,
    );
    let diagnostics = analyzed::accumulated::<DiagnosticAccumulator>(&db, source);
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.0.code_text() == Some("KSEM003")),
        "`Widgets` declaring its own `Color` shadows `Palette`'s: {diagnostics:?}"
    );
}

/// A qualifier naming a package that declares nothing by that name resolves to
/// what the file itself can see.
///
/// `Views` speaks `Color` without declaring one, so `Views.Color` is the `Color`
/// that `Views`'s own API means — the one `Palette` declares. `Widgets` declares
/// no `Color` of its own (declaring one would shadow the import and be refused),
/// so the qualifier's fall-through is what is under test here.
#[test]
fn a_qualifier_resolves_past_a_package_that_declares_no_such_name() {
    let db = salsa::DatabaseImpl::new();
    let mut modules = palette_modules();
    modules.push(ModuleSource {
        module: ImportTable::package_module_identity("Widgets", "Widgets"),
        path: "Widgets/Widgets.kira".to_owned(),
        text: "import Palette\nimport Views\n\
               function viewFill() -> Views.Color { return Palette.Color { rgba: 1 } }\n\
               function makeView() -> View { return View { fill: viewFill() } }"
            .to_owned(),
    });
    let source = SourceProgram::application(
        &db,
        "import Widgets\n@Main function main() { print(makeView().fill.rgba) return }".to_owned(),
        "main.kira".to_owned(),
        modules,
    );
    let diagnostics = analyzed::accumulated::<DiagnosticAccumulator>(&db, source);
    assert!(
        diagnostics.is_empty(),
        "`Views.Color` is the `Color` `Views` speaks, which is `Palette`'s: {diagnostics:?}"
    );
}

/// The fall-through does not let visibility compose.
///
/// `Widgets` importing `Views` does not lend it `Palette`, so `Views.Color`
/// resolves to nothing a file that never imported `Palette` can name.
#[test]
fn a_qualifier_does_not_reach_a_package_this_file_never_imported() {
    let db = salsa::DatabaseImpl::new();
    let mut modules = palette_modules();
    modules.push(ModuleSource {
        module: ImportTable::package_module_identity("Widgets", "Widgets"),
        path: "Widgets/Widgets.kira".to_owned(),
        text: "import Views\nfunction viewFill(color: Views.Color) -> Int { return 0 }".to_owned(),
    });
    let source = SourceProgram::application(
        &db,
        "import Widgets\n@Main function main() { return }".to_owned(),
        "main.kira".to_owned(),
        modules,
    );
    let diagnostics = analyzed::accumulated::<DiagnosticAccumulator>(&db, source);
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.0.has_code("KSEM050")),
        "`Views` importing `Palette` does not lend `Palette` to whoever imports \
         `Views`: {diagnostics:?}"
    );
}

/// Equal relative module names retain their package identity for both imports.
#[test]
fn same_named_modules_in_two_packages_link_to_their_own_sources() {
    let db = salsa::DatabaseImpl::new();
    let first_root = "import Services\nfunction firstRoot() -> Int { return firstService() }";
    let second_root = "import Services\nfunction secondRoot() -> Int { return secondService() }";
    let modules = vec![
        ModuleSource {
            module: ImportTable::package_module_identity("First", "Services"),
            path: "First/Services.kira".to_owned(),
            text: "function firstService() -> Int { return 1 }".to_owned(),
        },
        ModuleSource {
            module: ImportTable::package_module_identity("First", "First"),
            path: "First/First.kira".to_owned(),
            text: first_root.to_owned(),
        },
        ModuleSource {
            module: ImportTable::package_module_identity("Second", "Services"),
            path: "Second/Services.kira".to_owned(),
            text: "function secondService() -> Int { return 2 }".to_owned(),
        },
        ModuleSource {
            module: ImportTable::package_module_identity("Second", "Second"),
            path: "Second/Second.kira".to_owned(),
            text: second_root.to_owned(),
        },
    ];
    let source = SourceProgram::application(
        &db,
        "import First\nimport Second\n@Main function main() { return }".to_owned(),
        "main.kira".to_owned(),
        modules,
    );

    let diagnostics = analyzed::accumulated::<DiagnosticAccumulator>(&db, source);
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
    let links = analyzed::accumulated::<DefinitionAccumulator>(&db, source);
    let services_span = Span::new(7, "Services".len() as u32);
    let first_link = links
        .iter()
        .find(|link| link.0.reference == FileSpan::new(module_source_id(1), services_span))
        .expect("the first package import records a link");
    let second_link = links
        .iter()
        .find(|link| link.0.reference == FileSpan::new(module_source_id(3), services_span))
        .expect("the second package import records a link");

    assert_eq!(
        first_link.0.definition,
        FileSpan::new(module_source_id(0), Span::new(0, 0))
    );
    assert_eq!(
        second_link.0.definition,
        FileSpan::new(module_source_id(2), Span::new(0, 0))
    );
}

/// A module's declarations are visible bare across the package: an import puts
/// the file in the program and binds a namespace root, and gates nothing.
#[test]
fn a_modules_declarations_are_visible_bare() {
    let diagnostics = module_diagnostics(
        "import support\n\
         @Main function main() { let p = SupportPoint { x: 1, y: 2 } print(p.y) return }",
        &[("support", SUPPORT)],
    );
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
}

/// A diagnostic raised inside a module points into *that* module's file, not
/// into the entry file — which is what makes a multi-file error readable.
#[test]
fn a_modules_diagnostic_points_into_the_module() {
    let diagnostics = module_diagnostics(
        "import broken\n@Main function main() { print(1) return }",
        &[("broken", "function bad() -> Int { return nope }")],
    );
    let diagnostic = diagnostics
        .iter()
        .find(|diagnostic| diagnostic.has_code("KSEM060"))
        .expect("the module's undefined name is reported");
    let label = diagnostic.labels.first().expect("a span to point at");
    assert_eq!(label.span.source, module_source_id(0));
}
