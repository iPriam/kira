//! Parser tests for `type Name = Target`: the shape, and the recovery that
//! keeps a malformed alias from derailing the rest of the file.

use super::{parse_text, type_spelling};
use crate::ParseResult;
use kira_syntax_model::ast::{Item, TypeAliasDecl};

/// The aliases in `text`, in source order.
fn aliases(result: &ParseResult) -> Vec<&TypeAliasDecl> {
    result
        .tree
        .items()
        .iter()
        .filter_map(|item| match item {
            Item::TypeAlias(declaration) => Some(declaration),
            _ => None,
        })
        .collect()
}

#[test]
fn an_alias_records_its_name_and_target() {
    let result = parse_text("type Count = Int");
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
    let declared = aliases(&result);
    assert_eq!(declared.len(), 1);
    assert_eq!(result.interner.resolve(declared[0].name), "Count");
    assert_eq!(type_spelling(&result, declared[0].target), "Int");
}

/// The target is an ordinary type reference, so nesting comes for free.
#[test]
fn an_alias_target_may_be_an_array_of_any_depth() {
    let result = parse_text("type Buffer = [Byte]\ntype Matrix = [[Byte]]");
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
    let declared = aliases(&result);
    assert_eq!(type_spelling(&result, declared[0].target), "[Byte]");
    assert_eq!(type_spelling(&result, declared[1].target), "[[Byte]]");
}

#[test]
fn aliases_sit_beside_the_other_items() {
    let result = parse_text("type Count = Int\nfunction f(n: Count) { return }");
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
    assert!(matches!(result.tree.items()[0], Item::TypeAlias(_)));
    assert!(matches!(result.tree.items()[1], Item::Function(_)));
}

/// `type` is a keyword, so it can no longer be an identifier — and a
/// declaration missing its name is reported rather than silently dropped.
#[test]
fn an_alias_without_a_name_is_reported_and_recovery_continues() {
    let result = parse_text("type = Int\nfunction f() { return }");
    let codes: Vec<_> = result
        .diagnostics
        .iter()
        .filter_map(kira_diagnostics::Diagnostic::code_text)
        .collect();
    assert!(codes.contains(&"KPAR032"), "{codes:?}");
    assert!(aliases(&result).is_empty(), "no alias node was built");
    assert!(
        result
            .tree
            .items()
            .iter()
            .any(|item| matches!(item, Item::Function(_))),
        "the function after the bad alias still parsed",
    );
}

/// `=` is required: `type Name` alone aliases nothing, so the missing `=` is
/// reported and the next item still parses.
#[test]
fn an_alias_without_an_equals_is_reported_and_recovery_continues() {
    let result = parse_text("type Count\nfunction f() { return }");
    let codes: Vec<_> = result
        .diagnostics
        .iter()
        .filter_map(kira_diagnostics::Diagnostic::code_text)
        .collect();
    assert!(codes.contains(&"KPAR001"), "{codes:?}");
    assert!(
        result
            .tree
            .items()
            .iter()
            .any(|item| matches!(item, Item::Function(_))),
        "the function after the bad alias still parsed",
    );
}

/// A malformed target leaves an error type reference rather than aborting.
#[test]
fn an_alias_with_a_malformed_target_yields_an_error_type() {
    let result = parse_text("type Count = 5\nfunction f() { return }");
    let declared = aliases(&result);
    assert_eq!(declared.len(), 1);
    assert_eq!(type_spelling(&result, declared[0].target), "<error>");
    assert!(
        result
            .tree
            .items()
            .iter()
            .any(|item| matches!(item, Item::Function(_))),
    );
}
