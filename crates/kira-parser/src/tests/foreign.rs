//! Parsing the `@FFI.Extern` marker and the bodyless extern it rides on.
//!
//! The parser's whole job here is to record what was written — the qualified
//! annotation name, the `key: value;` block, and the terminating `;` in place
//! of a body — and to keep the file parsing when any of that is malformed.
//! Deciding what the fields *mean* (required, duplicate, the `abi` value) is
//! semantics', so those cases live in the semantics tests.

use kira_diagnostics::Diagnostic;

use super::*;
use kira_syntax_model::ast::{FfiTypeKind, ForeignMark};

/// The foreign marker of the one function in `text`.
fn only_foreign(result: &ParseResult) -> &ForeignMark {
    only_function(result)
        .foreign
        .as_ref()
        .expect("the function carries an `@FFI.Extern` marker")
}

/// The diagnostic codes `text` produced, in order.
fn codes(result: &ParseResult) -> Vec<&str> {
    result
        .diagnostics
        .iter()
        .filter_map(Diagnostic::code_text)
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

/// The `@FFI.*` type mark of the one struct in `text`.
fn only_ffi_kind(result: &ParseResult) -> &FfiTypeKind {
    &only_struct(result)
        .ffi
        .as_ref()
        .expect("the struct carries an `@FFI.*` type mark")
        .kind
}

#[test]
fn ffi_struct_parses_layout_and_keeps_the_body() {
    let result = parse_text(
        "@FFI.Struct { layout: c; }\n\
         struct Color {\n    var r: U8\n    var g: U8\n}",
    );
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
    let declaration = only_struct(&result);
    assert_eq!(declaration.fields.len(), 2);
    match only_ffi_kind(&result) {
        FfiTypeKind::Struct { layout } => {
            let (symbol, _) = layout.expect("layout recorded");
            assert_eq!(result.interner.resolve(symbol), "c");
        }
        other => panic!("expected Struct, got {other:?}"),
    }
}

#[test]
fn ffi_pointer_parses_target_and_ownership() {
    let result =
        parse_text("@FFI.Pointer { target: Color; ownership: borrowed; }\nstruct Color_ptr {}");
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
    match only_ffi_kind(&result) {
        FfiTypeKind::Pointer { target, ownership } => {
            assert!(target.is_some());
            let (symbol, _) = ownership.expect("ownership recorded");
            assert_eq!(result.interner.resolve(symbol), "borrowed");
        }
        other => panic!("expected Pointer, got {other:?}"),
    }
}

#[test]
fn ffi_alias_parses_a_plain_target() {
    let result = parse_text("@FFI.Alias { target: U64; }\nstruct Address {}");
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
    match only_ffi_kind(&result) {
        FfiTypeKind::Alias { target } => assert!(target.is_some()),
        other => panic!("expected Alias, got {other:?}"),
    }
}

#[test]
fn ffi_alias_tolerates_a_union_tag_on_the_target() {
    let result = parse_text("@FFI.Alias { target: union Version; }\nstruct Version {}");
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
    match only_ffi_kind(&result) {
        FfiTypeKind::Alias { target } => assert!(target.is_some()),
        other => panic!("expected Alias, got {other:?}"),
    }
}

#[test]
fn ffi_array_parses_element_and_count() {
    let result = parse_text("@FFI.Array { element: U8; count: 8; }\nstruct Bytes8 {}");
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
    match only_ffi_kind(&result) {
        FfiTypeKind::Array { element, count } => {
            assert!(element.is_some());
            let (value, _) = count.expect("count recorded");
            assert_eq!(value, 8);
        }
        other => panic!("expected Array, got {other:?}"),
    }
}

#[test]
fn ffi_callback_parses_params_and_result() {
    let result = parse_text(
        "@FFI.Callback { abi: c; params: [I32, RawPtr]; result: Void; }\nstruct Handler {}",
    );
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
    match only_ffi_kind(&result) {
        FfiTypeKind::Callback {
            params, result: r, ..
        } => {
            assert_eq!(params.len(), 2);
            assert!(r.is_some());
        }
        other => panic!("expected Callback, got {other:?}"),
    }
}

#[test]
fn ffi_callback_parses_an_empty_params_list() {
    let result = parse_text("@FFI.Callback { abi: c; params: []; result: Int; }\nstruct Thunk {}");
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
    match only_ffi_kind(&result) {
        FfiTypeKind::Callback { params, .. } => assert!(params.is_empty()),
        other => panic!("expected Callback, got {other:?}"),
    }
}

#[test]
fn an_ffi_type_annotation_on_a_function_is_rejected() {
    let result = parse_text("@FFI.Struct { layout: c; }\nfunction f() -> I32 { return 0 }");
    assert!(codes(&result).contains(&"KPAR056"), "{:?}", codes(&result));
}

#[test]
fn an_ffi_extern_on_a_struct_is_rejected() {
    let result = parse_text("@FFI.Extern { library: l; symbol: s; abi: c; }\nstruct S {}");
    assert!(codes(&result).contains(&"KPAR056"), "{:?}", codes(&result));
}

#[test]
fn an_empty_block_field_span_points_at_the_offending_token() {
    // The `;` is missing, so KPAR052 must point at the `}` that stands where the
    // `;` should be — the token the author would fix.
    let result = parse_text("@FFI.Extern { library: ffimath }\nfunction add() -> I32;");
    let diagnostic = result
        .diagnostics
        .iter()
        .find(|diagnostic| diagnostic.has_code("KPAR052"))
        .expect("a missing-semicolon diagnostic");
    let span = diagnostic.labels[0].span.span;
    assert_eq!(span.slice("@FFI.Extern { library: ffimath }"), "}");
}
