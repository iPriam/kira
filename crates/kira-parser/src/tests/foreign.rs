//! Parsing the `@FFI.Extern` marker and the bodyless extern it rides on.
//!
//! The parser's whole job here is to record what was written — the qualified
//! annotation name, the `key: value;` block, and the terminating `;` in place
//! of a body — and to keep the file parsing when any of that is malformed.
//! Deciding what the fields *mean* (required, duplicate, the `abi` value) is
//! semantics', so those cases live in the semantics tests.

use super::*;
use kira_syntax_model::ast::ForeignMark;

/// The foreign marker of the one function in `text`.
fn only_foreign(result: &ParseResult) -> &ForeignMark {
    only_function(result)
        .foreign
        .as_ref()
        .expect("the function carries an `@FFI.Extern` marker")
}

/// The diagnostic codes `text` produced, in order.
fn codes(result: &ParseResult) -> Vec<&'static str> {
    result
        .diagnostics
        .iter()
        .filter_map(|diagnostic| diagnostic.code)
        .collect()
}

#[test]
fn a_bodyless_extern_parses_with_its_block() {
    let result = parse_text(
        "@FFI.Extern { library: ffimath; symbol: kira_ffi_add; abi: c; }\n\
         function add(a: I32, b: I32) -> I32;",
    );
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
    let function = only_function(&result);
    // Bodyless: the stored body carries no statements.
    assert!(function.body.stmts.is_empty());
    assert_eq!(function.params.len(), 2);
    assert!(function.return_type.is_some());
    let mark = only_foreign(&result);
    let fields: Vec<(String, String)> = mark
        .fields
        .iter()
        .map(|field| {
            (
                result.interner.resolve(field.key).to_owned(),
                result.interner.resolve(field.value).to_owned(),
            )
        })
        .collect();
    assert_eq!(
        fields,
        vec![
            ("library".to_owned(), "ffimath".to_owned()),
            ("symbol".to_owned(), "kira_ffi_add".to_owned()),
            ("abi".to_owned(), "c".to_owned()),
        ]
    );
}

#[test]
fn an_ordinary_function_carries_no_foreign_marker() {
    let result = parse_text("function add(a: Int, b: Int) -> Int { return a + b }");
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
    assert!(only_function(&result).foreign.is_none());
}

#[test]
fn a_bodyless_ordinary_function_is_rejected() {
    let result = parse_text("function f();");
    assert_eq!(codes(&result), vec!["KPAR055"]);
    // The parser still yields a usable function: it never bails.
    assert!(matches!(result.tree.items(), [Item::Function(_)]));
}

#[test]
fn an_extern_with_a_body_is_rejected() {
    let result = parse_text(
        "@FFI.Extern { library: l; symbol: s; abi: c; }\n\
         function add(a: I32) -> I32 { return a }",
    );
    assert_eq!(codes(&result), vec!["KPAR054"]);
    // The marker is still recorded, so semantics can point at the declaration.
    assert!(only_function(&result).foreign.is_some());
}

#[test]
fn a_missing_block_brace_is_rejected() {
    let result = parse_text("@FFI.Extern function add(a: I32) -> I32;");
    assert_eq!(codes(&result), vec!["KPAR048"]);
}

#[test]
fn a_non_identifier_field_key_is_rejected() {
    let result = parse_text("@FFI.Extern { 42: x; }\nfunction add() -> I32;");
    assert!(codes(&result).contains(&"KPAR049"), "{:?}", codes(&result));
}

#[test]
fn a_missing_field_colon_is_rejected() {
    let result = parse_text("@FFI.Extern { library ffimath; }\nfunction add() -> I32;");
    assert!(codes(&result).contains(&"KPAR050"), "{:?}", codes(&result));
}

#[test]
fn a_non_identifier_field_value_is_rejected() {
    let result = parse_text("@FFI.Extern { library: 42; }\nfunction add() -> I32;");
    assert!(codes(&result).contains(&"KPAR051"), "{:?}", codes(&result));
}

#[test]
fn a_missing_field_semicolon_is_rejected() {
    let result = parse_text("@FFI.Extern { library: ffimath }\nfunction add() -> I32;");
    assert!(codes(&result).contains(&"KPAR052"), "{:?}", codes(&result));
}

#[test]
fn an_unknown_qualified_annotation_is_rejected() {
    let result = parse_text("@FFI.Import { library: l; }\nfunction add() -> I32;");
    assert!(codes(&result).contains(&"KPAR053"), "{:?}", codes(&result));
    // A dotted name other than `FFI.Extern` records no foreign marker.
    assert!(only_function(&result).foreign.is_none());
}

#[test]
fn an_empty_block_field_span_points_at_the_offending_token() {
    // The `;` is missing, so KPAR052 must point at the `}` that stands where the
    // `;` should be — the token the author would fix.
    let result = parse_text("@FFI.Extern { library: ffimath }\nfunction add() -> I32;");
    let diagnostic = result
        .diagnostics
        .iter()
        .find(|diagnostic| diagnostic.code == Some("KPAR052"))
        .expect("a missing-semicolon diagnostic");
    let span = diagnostic.labels[0].span.span;
    assert_eq!(span.slice("@FFI.Extern { library: ffimath }"), "}");
}
