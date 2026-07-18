//! File-scoped import resolution: what an import brings into scope, what it
//! does not, and what it refuses.

use super::{module_codes, module_diagnostics};

/// The module `support.kira` most cases here import.
const SUPPORT: &str = "function supportValue() -> Int { return 42 }\n\
                       struct SupportPoint { let x: Int  let y: Int }\n";

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
    assert_eq!(label.span.source, crate::module_source_id(0));
}
