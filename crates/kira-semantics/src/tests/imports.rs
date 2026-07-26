//! File-scoped import resolution: what an import brings into scope, what it
//! does not, and what it refuses.

use super::{codes, module_codes, module_diagnostics};
use crate::{
    DefinitionAccumulator, DiagnosticAccumulator, ImportTable, ModuleSource, SourceProgram,
    analyzed, module_source_id,
};
use kira_semantics_model::{Callee, HirExpr, HirProgram, HirStmt};
use kira_source::{FileSpan, Span};

/// The module `support.kira` most cases here import.
const SUPPORT: &str = "function supportValue() -> Int { return 42 }\n\
                       struct SupportPoint { let x: Int  let y: Int }\n";

/// The analyzed HIR of a program built from an entry file plus named modules.
fn module_program(text: &str, modules: &[(&str, &str)]) -> HirProgram {
    let db = salsa::DatabaseImpl::new();
    let modules: Vec<ModuleSource> = modules
        .iter()
        .map(|&(module, text)| ModuleSource {
            module: module.to_owned(),
            path: format!("{module}.kira"),
            text: text.to_owned(),
        })
        .collect();
    let source = SourceProgram::application(&db, text.to_owned(), "test.kira".to_owned(), modules);
    analyzed(&db, source)
}

#[test]
fn an_imported_module_is_part_of_the_program() {
    let diagnostics = module_diagnostics(
        "import support\n@Main function main() { print(supportValue()) return }",
        &[("support", SUPPORT)],
    );
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
}

/// The alias is the namespace root, and the qualified call resolves through it.
#[test]
fn an_alias_names_the_module_for_qualified_calls() {
    let diagnostics = module_diagnostics(
        "import support as Support\n\
         @Main function main() { print(Support.supportValue()) return }",
        &[("support", SUPPORT)],
    );
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
}

/// Without an alias the last path segment is the root, so both spellings work.
#[test]
fn an_unaliased_import_binds_its_own_name_as_the_root() {
    let diagnostics = module_diagnostics(
        "import support\n@Main function main() { print(support.supportValue()) return }",
        &[("support", SUPPORT)],
    );
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
}

/// A module-qualified *type* names the same type the module declares bare.
#[test]
fn a_qualified_type_resolves_through_the_alias() {
    let diagnostics = module_diagnostics(
        "import support as Support\n\
         @Main function main() { let p: Support.SupportPoint = SupportPoint { x: 1, y: 2 } \
         print(p.x) return }",
        &[("support", SUPPORT)],
    );
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
}

/// A dotted module path is a real path, and its last segment is the root.
#[test]
fn a_dotted_module_path_binds_its_last_segment() {
    let diagnostics = module_diagnostics(
        "import Foundation.Web as Web\n@Main function main() { print(Web.webValue()) return }",
        &[("Foundation.Web", "function webValue() -> Int { return 7 }")],
    );
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
}

/// A module-qualified struct literal names the same struct the module declares
/// bare, so `Support.SupportPoint { … }` constructs it exactly as
/// `SupportPoint { … }` does.
#[test]
fn a_qualified_struct_literal_resolves_through_the_alias() {
    let diagnostics = module_diagnostics(
        "import support as Support\n\
         @Main function main() { let p = Support.SupportPoint { x: 1, y: 2 } print(p.x) return }",
        &[("support", SUPPORT)],
    );
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
}

/// A module a file did not import cannot qualify a struct literal: the qualifier
/// is refused with the same file-scope diagnostic a qualified type gets.
#[test]
fn a_qualified_struct_literal_needs_this_files_import() {
    let codes = module_codes(
        "import support\nimport leak\n@Main function main() { print(leakValue()) return }",
        &[
            ("support", SUPPORT),
            (
                "leak",
                "function leakValue() -> Int { let p = support.SupportPoint { x: 1, y: 2 } \
                 return p.x }",
            ),
        ],
    );
    assert_eq!(codes, vec!["KSEM027"]);
}

/// A qualified enum variant is written with a spelling rather than a leading
/// dot: `Support.Tone.Warm` and the bare `Tone.Warm` both name the variant, and
/// a payload variant carries its argument through the same path.
#[test]
fn a_qualified_enum_variant_resolves_bare_and_module_qualified() {
    let diagnostics = module_diagnostics(
        "import support as Support\n\
         @Main function main() { \
         let a = Support.Tone.Warm  let b = Tone.Cool  let c = Support.Tone.Level(3) \
         print(rank(a) + rank(b) + rank(c)) return }",
        &[(
            "support",
            "enum Tone { Warm  Cool  Level(Int) }\n\
             function rank(t: borrow Tone) -> Int { if t == .Warm { return 1 } return 2 }",
        )],
    );
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
}

/// A qualified spelling of a variant that does not exist is refused with the
/// enum's own typed diagnostic, not a silent pass.
#[test]
fn an_unknown_qualified_enum_variant_is_refused() {
    let codes = module_codes(
        "import support as Support\n\
         @Main function main() { let a = Support.Tone.Nope  return }",
        &[("support", "enum Tone { Warm  Cool }")],
    );
    assert_eq!(codes, vec!["KSEM120"]);
}

/// The headline rule: an import written in one file does not carry into
/// another. `leak.kira` never imported `support`, so its qualified reference
/// has no namespace root to resolve through — even though the entry file's
/// import put `support` in the program.
#[test]
fn a_siblings_import_does_not_carry_over() {
    let codes = module_codes(
        "import support\nimport leak\n@Main function main() { leakValue() return }",
        &[
            ("support", SUPPORT),
            (
                "leak",
                "function leakValue() { print(support.supportValue()) return }",
            ),
        ],
    );
    assert_eq!(codes, vec!["KSEM027"], "the sibling's import does not leak");
}

/// The same rule for types: a qualified type name needs *this* file's import.
#[test]
fn a_siblings_import_does_not_carry_over_to_a_type() {
    let codes = module_codes(
        "import support\nimport leak\n@Main function main() { print(leakValue()) return }",
        &[
            ("support", SUPPORT),
            (
                "leak",
                "function leakValue() -> Int { let p: support.SupportPoint = \
                 SupportPoint { x: 1, y: 2 } return p.x }",
            ),
        ],
    );
    assert_eq!(codes, vec!["KSEM027"]);
}

/// An import naming a module the program was not built from is unresolved.
#[test]
fn an_import_of_a_missing_module_is_reported() {
    let codes = module_codes(
        "import missing\n@Main function main() { print(1) return }",
        &[],
    );
    assert_eq!(codes, vec!["KSEM032"]);
}

/// `Foundation` is not shipped here, so importing it says so rather than
/// resolving to nothing quietly.
#[test]
fn importing_foundation_reports_that_there_is_no_such_module() {
    let diagnostics = module_diagnostics(
        "import Foundation\n@Main function main() { print(1) return }",
        &[],
    );
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0].code, Some("KSEM032"));
    assert!(
        diagnostics[0].message.contains("Foundation"),
        "{}",
        diagnostics[0].message
    );
}

/// The flat-package rule for types: a struct field may name a struct declared
/// in another file of the program, with no import and regardless of load order.
/// The entry file's `World` holds a `SpatialBvh` declared in a later sibling —
/// two-phase collection registers every name before resolving any field, so the
/// forward reference across files resolves instead of raising KSEM051.
#[test]
fn a_struct_field_may_name_a_struct_in_a_later_sibling_file() {
    let diagnostics = module_diagnostics(
        "struct World { var bvh: SpatialBvh  var tick: Int }\n\
         @Main function main() { let w = World { bvh: SpatialBvh { depth: 0 }, tick: 1 } \
         print(w.bvh.depth + w.tick) return }",
        &[("bvh", "struct SpatialBvh { var depth: Int }")],
    );
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
}

/// Field defaults resolve once in their declaring file, not in each file that
/// constructs the struct. The entry imports neither `helper` nor the local
/// callback function, but omitting both fields still resolves cleanly.
#[test]
fn struct_defaults_resolve_in_the_defining_module() {
    let diagnostics = module_diagnostics(
        "import definitions\n\
         @Main function main() { let h = CallbackHolder {} print(h.seed) \
         print(h.callback()) return }",
        &[
            ("helper", "function helperValue() -> Int { return 41 }"),
            (
                "definitions",
                "import helper as H\n\
                 function moduleDefault() -> Int { return H.helperValue() + 1 }\n\
                 struct CallbackHolder {\n\
                     let seed: Int = H.helperValue()\n\
                     let callback: () -> Int = moduleDefault\n\
                 }",
            ),
        ],
    );
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
}

#[test]
fn a_resolved_struct_default_reuses_the_declaring_modules_callee() {
    let program = module_program(
        "import definitions\n\
         @Main function main() { let h = Holder {} print(h.value) return }",
        &[
            ("helper", "function helperValue() -> Int { return 41 }"),
            (
                "definitions",
                "import helper as H\nstruct Holder { let value: Int = H.helperValue() }",
            ),
        ],
    );
    let main = program.main.expect("main is recorded");
    let statement = program.functions[main.0 as usize]
        .body
        .first()
        .expect("main constructs the holder");
    let HirStmt::Let { init, .. } = program.stmt(*statement) else {
        panic!("main's first statement is the holder binding");
    };
    let HirExpr::StructNew { fields, .. } = program.expr(*init) else {
        panic!("the holder binding constructs the struct");
    };
    let HirExpr::Call {
        callee: Callee::User(callee),
        ..
    } = program.expr(fields[0])
    else {
        panic!("the omitted field reuses its resolved call default");
    };
    assert_eq!(program.functions[callee.0 as usize].name, "helperValue");
}

/// Resolving defaults at declaration time must not turn a genuinely missing
/// name into a delayed or construction-dependent error.
#[test]
fn an_undefined_name_in_an_unused_struct_default_is_refused() {
    assert_eq!(
        module_codes(
            "import broken\n@Main function main() { return }",
            &[("broken", "struct Broken { let value: Int = missing }")],
        ),
        vec!["KSEM060"]
    );
}

#[test]
fn recursively_constructing_struct_defaults_are_refused() {
    assert_eq!(
        codes(
            "struct A { let values: [B] = [B {}] }\n\
             struct B { let values: [A] = [A {}] }\n\
             @Main function main() { return }"
        ),
        vec!["KSEM213"]
    );
}

/// A root that is not a module at all keeps its old diagnostic: this is a
/// method call on an undefined name, not an import problem, and saying
/// "unresolved namespace" would send the reader looking for an import to add.
#[test]
fn a_root_that_is_no_module_is_still_an_undefined_name() {
    let codes = module_codes(
        "@Main function main() { print(Nope.value()) return }",
        &[("support", SUPPORT)],
    );
    assert_eq!(codes, vec!["KSEM060"]);
}

/// A local wins over a module of the same name, so importing a module never
/// makes a variable unusable.
#[test]
fn a_local_shadows_a_module_root() {
    let diagnostics = module_diagnostics(
        "import support as p\n\
         struct Pt { let x: Int  function get() -> Int { return x } }\n\
         @Main function main() { let p = Pt { x: 5 } print(p.get()) return }",
        &[("support", SUPPORT)],
    );
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
}

/// Two modules that import each other are a legal program, not a cycle error.
/// The reference implementation accepts this — its loader is visited-set
/// guarded — so inventing a rejection here would break a working program.
#[test]
fn mutually_importing_modules_are_accepted() {
    let diagnostics = module_diagnostics(
        "import alpha\n@Main function main() { print(alphaValue()) return }",
        &[
            (
                "beta",
                "import alpha\nfunction betaValue() -> Int { return alphaBase() }",
            ),
            (
                "alpha",
                "import beta\nfunction alphaBase() -> Int { return 3 }\n\
                 function alphaValue() -> Int { return betaValue() + 1 }",
            ),
        ],
    );
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
}

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
            .any(|diagnostic| diagnostic.0.code == Some("KSEM061")),
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
            .any(|diagnostic| diagnostic.0.code == Some("KSEM061")),
        "`Outer` importing `Inner` does not lend `Inner` to whoever imports `Outer`: \
         {diagnostics:?}"
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
        .find(|diagnostic| diagnostic.code == Some("KSEM060"))
        .expect("the module's undefined name is reported");
    let label = diagnostic.labels.first().expect("a span to point at");
    assert_eq!(label.span.source, module_source_id(0));
}
