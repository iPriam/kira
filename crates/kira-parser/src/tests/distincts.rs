//! Parser tests for `distinct Name = Representation`: the shape it records, and
//! the recovery that keeps a malformed one from derailing the rest of the file.

use super::{parse_text, type_spelling};
use crate::ParseResult;
use kira_syntax_model::ast::{DistinctDecl, Item};

/// The `distinct` declarations in `text`, in source order.
fn declared(result: &ParseResult) -> Vec<&DistinctDecl> {
    result
        .tree
        .items()
        .iter()
        .filter_map(|item| match item {
            Item::Distinct(declaration) => Some(declaration),
            _ => None,
        })
        .collect()
}

#[test]
fn a_distinct_declaration_records_its_name_and_representation() {
    let result = parse_text("distinct TabId = U32");
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
    let items = declared(&result);
    assert_eq!(items.len(), 1);
    assert_eq!(result.interner.resolve(items[0].name), "TabId");
    assert_eq!(type_spelling(&result, items[0].representation), "U32");
}

/// Two declarations over one representation are two declarations. The parser
/// records both; that they are two *types* is semantics' answer.
#[test]
fn two_declarations_over_one_representation_both_parse() {
    let result = parse_text("distinct TabId = U32\ndistinct BookmarkId = U32");
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
    let items = declared(&result);
    assert_eq!(items.len(), 2);
    assert_eq!(result.interner.resolve(items[0].name), "TabId");
    assert_eq!(result.interner.resolve(items[1].name), "BookmarkId");
}

#[test]
fn a_distinct_declaration_sits_beside_the_other_items() {
    let result = parse_text("distinct TabId = U32\nfunction f(id: TabId) { return }");
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
    assert!(matches!(result.tree.items()[0], Item::Distinct(_)));
    assert!(matches!(result.tree.items()[1], Item::Function(_)));
}

/// `distinct` is a keyword, so a declaration missing its name is reported
/// rather than silently dropped — and the file keeps parsing.
#[test]
fn a_declaration_missing_its_name_is_reported_and_recovered_from() {
    let result = parse_text("distinct = U32\nfunction f() { return }");
    assert_eq!(
        result
            .diagnostics
            .iter()
            .filter_map(kira_diagnostics::Diagnostic::code_text)
            .collect::<Vec<_>>(),
        vec!["KPAR084"]
    );
    assert!(declared(&result).is_empty());
    assert!(matches!(result.tree.items()[0], Item::Function(_)));
}

/// A declaration with no `=` names a type with no representation, which is a
/// type nothing could build a value of.
#[test]
fn a_declaration_missing_its_representation_is_reported() {
    let result = parse_text("distinct TabId\nfunction f() { return }");
    assert_eq!(
        result
            .diagnostics
            .iter()
            .filter_map(kira_diagnostics::Diagnostic::code_text)
            .collect::<Vec<_>>(),
        vec!["KPAR085"]
    );
    assert!(declared(&result).is_empty());
}

/// `@Derive(Copy)` claims nothing about a type that is one scalar word, so it is
/// refused by name — and the declaration is still recorded, because dropping it
/// would turn one refusal into an unresolved-type cascade at every use.
#[test]
fn derive_copy_on_a_distinct_declaration_is_refused_by_name() {
    let result = parse_text("@Derive(Copy)\ndistinct TabId = U32");
    assert_eq!(
        result
            .diagnostics
            .iter()
            .filter_map(kira_diagnostics::Diagnostic::code_text)
            .collect::<Vec<_>>(),
        vec!["KPAR086"]
    );
    assert_eq!(declared(&result).len(), 1);
}

/// An execution or export marker says how a *function* runs or what a *library*
/// offers, so neither annotates a type declaration.
#[test]
fn an_execution_marker_on_a_distinct_declaration_is_refused() {
    let result = parse_text("@Native\ndistinct TabId = U32");
    assert_eq!(
        result
            .diagnostics
            .iter()
            .filter_map(kira_diagnostics::Diagnostic::code_text)
            .collect::<Vec<_>>(),
        vec!["KPAR087"]
    );
    assert_eq!(declared(&result).len(), 1);
}
