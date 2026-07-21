//! Import declarations, aliases, qualified paths, and recovery.

use crate::*;
use kira_syntax_model::ast::{Item, TypeRef};

// ----- imports ---------------------------------------------------------

#[test]
fn a_bare_import_parses_with_no_alias() {
    let result = parse(
        SourceId::new(0),
        "import support\n@Main function main() { return }",
    );
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
    match result.tree.items() {
        [Item::Import(declaration), Item::Function(_)] => {
            assert_eq!(declaration.path.len(), 1);
            assert_eq!(result.interner.resolve(declaration.path[0]), "support");
            assert!(declaration.alias.is_none());
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn an_aliased_import_records_its_alias() {
    let result = parse(
        SourceId::new(0),
        "import support as Support\n@Main function main() { return }",
    );
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
    match result.tree.items() {
        [Item::Import(declaration), ..] => {
            let alias = declaration.alias.expect("an alias was written");
            assert_eq!(result.interner.resolve(alias), "Support");
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_dotted_import_path_keeps_every_segment() {
    let result = parse(
        SourceId::new(0),
        "import Foundation.Web as Web\n@Main function main() { return }",
    );
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
    match result.tree.items() {
        [Item::Import(declaration), ..] => {
            let segments: Vec<&str> = declaration
                .path
                .iter()
                .map(|&segment| result.interner.resolve(segment))
                .collect();
            assert_eq!(segments, vec!["Foundation", "Web"]);
        }
        other => panic!("{other:?}"),
    }
}

/// Recovery: a malformed import must not derail the file. The import yields no
/// item — an import naming nothing would only produce a second, misleading
/// "unresolved module" — but the function after it still parses.
#[test]
fn a_malformed_import_still_leaves_the_rest_of_the_file_parsed() {
    let result = parse(
        SourceId::new(0),
        "import 42\n@Main function main() { print(1) return }",
    );
    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == Some("KPAR016")),
        "{:?}",
        result.diagnostics
    );
    assert!(
        result
            .tree
            .items()
            .iter()
            .any(|item| matches!(item, Item::Function(_))),
        "the function after the bad import still parses: {:?}",
        result.tree.items()
    );
}

/// `as` with no name after it is reported, and the import is still recorded —
/// the module is known, only the spelling of its root is not.
#[test]
fn an_alias_with_no_name_is_reported() {
    let result = parse(
        SourceId::new(0),
        "import support as\n@Main function main() { return }",
    );
    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == Some("KPAR017")),
        "{:?}",
        result.diagnostics
    );
}

/// A module-qualified type name is one `TypeRef::Named` whose symbol carries
/// the dot; a dot cannot appear in an identifier, so it can collide with
/// nothing a user declared.
#[test]
fn a_qualified_type_name_is_interned_with_its_qualifier() {
    let result = parse(SourceId::new(0), "function f(p: Support.Point) { return }");
    let function = match &result.tree.items()[0] {
        Item::Function(function) => function,
        other => panic!("{other:?}"),
    };
    match result.tree.type_ref(function.params[0].ty) {
        TypeRef::Named { name, .. } => {
            assert_eq!(result.interner.resolve(*name), "Support.Point");
        }
        other => panic!("{other:?}"),
    }
}
