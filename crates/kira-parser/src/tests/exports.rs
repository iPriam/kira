//! Parsing the `@Export` marker.
//!
//! The parser's whole job here is to record what was written — including a
//! payload `@Export` does not take — and to let a class carry the marker.
//! Deciding what any of it *means* is semantics'.

use super::*;
use kira_syntax_model::ast::ClassDecl;

/// The one class in `text`.
fn only_class(result: &ParseResult) -> &ClassDecl {
    match result.tree.items() {
        [Item::Class(declaration)] => declaration,
        items => panic!("expected exactly one class, got {items:?}"),
    }
}

#[test]
fn a_bare_export_marks_a_function_and_carries_no_payload() {
    let result = parse_text("@Export\nfunction makeButton(t: String) -> String { return t }");
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
    let mark = only_function(&result).export.expect("an export marker");
    assert_eq!(mark.payload_span, None);
}

#[test]
fn an_unmarked_function_carries_no_marker() {
    let result = parse_text("function add(a: Int) -> Int { return a }");
    assert!(only_function(&result).export.is_none());
}

#[test]
fn an_argument_list_after_export_is_recorded_rather_than_dropped() {
    // The parser does not refuse it — semantics does, against this span. What
    // matters here is that the payload survives instead of being skipped
    // silently, which is what would make the refusal unreportable.
    let result = parse_text("@Export(symbol)\nfunction add(a: Int) -> Int { return a }");
    let mark = only_function(&result).export.expect("an export marker");
    let payload = mark.payload_span.expect("a recorded payload");
    assert_eq!(&"@Export(symbol)"[payload.start as usize..], "(symbol)");
}

#[test]
fn an_annotation_block_after_export_is_recorded_rather_than_dropped() {
    let text = "@Export { symbol: uif_add; }\nfunction add(a: Int) -> Int { return a }";
    let result = parse_text(text);
    let mark = only_function(&result).export.expect("an export marker");
    let payload = mark.payload_span.expect("a recorded payload");
    let end = (payload.start + payload.len) as usize;
    assert_eq!(&text[payload.start as usize..end], "{ symbol: uif_add; }");
    // The body still parsed: a swallowed block would have eaten the function.
    assert_eq!(result.interner.resolve(only_function(&result).name), "add");
}

#[test]
fn export_marks_a_class() {
    let result = parse_text("@Export\nclass Button { var title: String = \"\" }");
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
    let declaration = only_class(&result);
    assert!(declaration.export.is_some());
    assert_eq!(result.interner.resolve(declaration.name), "Button");
    // The declaration's span starts at the annotation, not at `class`.
    assert_eq!(declaration.span.start, 0);
}

#[test]
fn an_unannotated_class_still_parses_and_carries_no_marker() {
    let result = parse_text("class Button { var title: String = \"\" }");
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
    assert!(only_class(&result).export.is_none());
}

#[test]
fn only_export_may_annotate_a_class() {
    // `@Main`, `@Runtime`, and `@Native` select how a *function* runs, so they
    // say nothing about a class. Refused rather than ignored.
    for text in [
        "@Main\nclass Button { var t: String = \"\" }",
        "@Native\nclass Button { var t: String = \"\" }",
        "@Runtime\nclass Button { var t: String = \"\" }",
    ] {
        let result = parse_text(text);
        let codes: Vec<_> = result
            .diagnostics
            .iter()
            .filter_map(|diagnostic| diagnostic.code)
            .collect();
        assert_eq!(codes, vec!["KPAR041"], "{text}");
    }
}

#[test]
fn only_export_may_annotate_a_struct() {
    // Same rule as the class arm, and the same failure it guards against: an
    // engine or entrypoint marker on a struct means nothing, so it is refused
    // rather than silently discarded. The struct still lands in the tree.
    for text in [
        "@Main\nstruct Point { let x: Int }",
        "@Native\nstruct Point { let x: Int }",
        "@Runtime\nstruct Point { let x: Int }",
    ] {
        let result = parse_text(text);
        let codes: Vec<_> = result
            .diagnostics
            .iter()
            .filter_map(|diagnostic| diagnostic.code)
            .collect();
        assert_eq!(codes, vec!["KPAR041"], "{text}");
        assert!(
            matches!(result.tree.items(), [Item::Struct(_)]),
            "the struct must still be registered: {:?}",
            result.tree.items()
        );
    }
}

#[test]
fn an_annotated_method_parses_as_a_method() {
    // The marker reaches semantics, which refuses a method export by name. It
    // must not land in the "expected a class member" arm, which would report
    // the wrong thing entirely.
    let result = parse_text(
        "class Button { var title: String = \"\"\n\
         @Export function label() -> String { return self.title } }",
    );
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
    let declaration = only_class(&result);
    assert_eq!(declaration.methods.len(), 1);
    assert!(declaration.methods[0].function.export.is_some());
}

#[test]
fn an_annotation_on_a_non_function_class_member_is_refused() {
    let result = parse_text("class Button { @Export var title: String = \"\" }");
    let codes: Vec<_> = result
        .diagnostics
        .iter()
        .filter_map(|diagnostic| diagnostic.code)
        .collect();
    assert!(codes.contains(&"KPAR042"), "{codes:?}");
}

#[test]
fn export_composes_with_the_engine_annotations() {
    let result = parse_text("@Export\n@Native\nfunction add(a: Int) -> Int { return a }");
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
    let function = only_function(&result);
    assert!(function.export.is_some());
    assert_eq!(function.execution, Execution::Native);
}

#[test]
fn a_struct_cannot_be_exported_and_is_still_registered() {
    // Only a class crosses, as a handle. Refused by name rather than falling
    // into the generic "not supported yet" arm — and the struct itself still
    // lands in the tree, so one refusal does not become an unresolved-type
    // cascade at every use of the name.
    let result = parse_text("@Export\nstruct Point { let x: Int }");
    let codes: Vec<_> = result
        .diagnostics
        .iter()
        .filter_map(|diagnostic| diagnostic.code)
        .collect();
    assert_eq!(codes, vec!["KPAR043"], "{codes:?}");
    assert!(
        matches!(result.tree.items(), [Item::Struct(_)]),
        "the struct must still be registered: {:?}",
        result.tree.items()
    );
}
