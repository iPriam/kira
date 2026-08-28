//! Type-parameter lists on declarations and type-argument lists on a use, plus
//! the recovery each malformed form falls back to.
//!
//! The one shape worth pinning here is the **shifted right angle**: the lexer
//! knows nothing about types, so a nested instantiation closes on a single
//! `>>` token that the parser splits in place. A test that only checked the
//! outer spelling would pass whether or not that split kept spans accurate, so
//! these check the nesting itself.

use crate::*;
use kira_syntax_model::ast::{Item, Stmt};

use super::{parse_text, type_spelling};

/// The one enum declaration in `text`.
fn only_enum(result: &ParseResult) -> &kira_syntax_model::ast::EnumDecl {
    match result.tree.items() {
        [Item::Enum(declaration)] => declaration,
        items => panic!("expected exactly one enum, got {items:?}"),
    }
}

/// The written type of the first `let` in the last item's body.
fn first_let_annotation(result: &ParseResult) -> String {
    let Some(Item::Function(function)) = result.tree.items().last() else {
        panic!("expected a trailing function");
    };
    for &id in &function.body.stmts {
        if let Stmt::Let { ty, .. } = result.tree.stmt(id)
            && let Some(annotation) = ty
        {
            return type_spelling(result, *annotation);
        }
    }
    panic!("expected an annotated `let`");
}

#[test]
fn the_oracles_result_parses_its_type_parameters() {
    let result = parse_text("enum Result<Value, Failure> {\n  Ok(Value)\n  Error(Failure)\n}");
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
    let declaration = only_enum(&result);
    let params: Vec<String> = declaration
        .type_params
        .iter()
        .map(|param| result.interner.resolve(param.name).to_owned())
        .collect();
    assert_eq!(params, ["Value", "Failure"]);
    // The payload types are the parameters, written as ordinary names.
    let ok = declaration.variants[0].payload.expect("a payload type");
    assert_eq!(type_spelling(&result, ok), "Value");
}

#[test]
fn an_ordinary_enum_declares_no_type_parameters() {
    let result = parse_text("enum Color { Red Green Blue }");
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
    assert!(only_enum(&result).type_params.is_empty());
}

#[test]
fn a_type_argument_list_parses_in_a_type_position() {
    let result = parse_text(
        "enum Result<V, F> { Ok(V) Error(F) }\n\
         @Main function main() { let x: Result<Int, String> = .Ok(1) return }",
    );
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
    assert_eq!(first_let_annotation(&result), "Result<Int, String>");
}

#[test]
fn a_nested_instantiation_splits_a_shifted_right_angle() {
    // `Result<Result<Int, String>, String>` closes on one `>>` token. Splitting
    // it in place is what lets both lists close, and the outer list must still
    // see exactly two arguments.
    let result = parse_text(
        "enum Result<V, F> { Ok(V) Error(F) }\n\
         @Main function main() { let x: Result<Result<Int, String>, String> = .Ok(1) return }",
    );
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
    assert_eq!(
        first_let_annotation(&result),
        "Result<Result<Int, String>, String>"
    );
}

#[test]
fn an_array_of_an_instantiation_parses() {
    let result = parse_text(
        "enum Result<V, F> { Ok(V) Error(F) }\n\
         @Main function main() { let x: [Result<Int, String>] = [] return }",
    );
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
    assert_eq!(first_let_annotation(&result), "[Result<Int, String>]");
}

#[test]
fn generic_struct_class_and_function_declarations_keep_their_parameters() {
    let struct_result = parse_text("struct Box<Value> { let v: Value }");
    assert!(
        struct_result.diagnostics.is_empty(),
        "{:?}",
        struct_result.diagnostics
    );
    let [Item::Struct(declaration)] = struct_result.tree.items() else {
        panic!("expected one generic struct");
    };
    assert_eq!(declaration.type_params.len(), 1);
    assert_eq!(
        struct_result
            .interner
            .resolve(declaration.type_params[0].name),
        "Value"
    );

    let class_result = parse_text("class Box<Value> { let v: Value }");
    assert!(
        class_result.diagnostics.is_empty(),
        "{:?}",
        class_result.diagnostics
    );
    let [Item::Class(declaration)] = class_result.tree.items() else {
        panic!("expected one generic class");
    };
    assert_eq!(declaration.type_params.len(), 1);
    assert_eq!(
        class_result
            .interner
            .resolve(declaration.type_params[0].name),
        "Value"
    );

    let function_result = parse_text("function id<Value>(value: Value) -> Value { return value }");
    assert!(
        function_result.diagnostics.is_empty(),
        "{:?}",
        function_result.diagnostics
    );
    let [Item::Function(function)] = function_result.tree.items() else {
        panic!("expected one generic function");
    };
    assert_eq!(function.type_params.len(), 1);
    assert_eq!(
        function_result
            .interner
            .resolve(function.type_params[0].name),
        "Value"
    );
}

#[test]
fn a_class_parent_records_explicit_generic_arguments() {
    let result = parse_text("class Child<Value> extends Parent<Value> {}");
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
    let [Item::Class(declaration)] = result.tree.items() else {
        panic!("expected one class");
    };
    assert_eq!(declaration.parents.len(), 1);
    assert_eq!(declaration.parents[0].type_args.len(), 1);
    assert_eq!(
        type_spelling(&result, declaration.parents[0].type_args[0]),
        "Value"
    );
}

#[test]
fn a_member_function_cannot_own_a_second_type_parameter_list() {
    let result =
        parse_text("struct Box { function get<Value>(value: Value) -> Value { return value } }\n");
    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.has_code("KPAR047")),
        "{:?}",
        result.diagnostics
    );
    let [Item::Struct(declaration)] = result.tree.items() else {
        panic!("expected a struct");
    };
    assert!(declaration.methods[0].type_params.is_empty());
}

#[test]
fn an_empty_parameter_list_is_reported_and_leaves_a_plain_enum() {
    let result = parse_text("enum Result<> { Ok Error }");
    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.has_code("KPAR046")),
        "{:?}",
        result.diagnostics,
    );
    assert!(only_enum(&result).type_params.is_empty());
}

#[test]
fn a_non_name_in_a_parameter_list_is_reported() {
    let result = parse_text("enum Result<Value, 12> { Ok(Value) }");
    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.has_code("KPAR044")),
        "{:?}",
        result.diagnostics,
    );
}

#[test]
fn an_unclosed_list_is_reported_rather_than_consuming_the_file() {
    let result = parse_text("enum Result<Value { Ok(Value) }");
    assert!(!result.diagnostics.is_empty());
    // The parser never bails: something still came out of the file.
    assert!(!result.tree.items().is_empty());
}

#[test]
fn a_bound_parses_onto_its_parameter() {
    let result = parse_text("enum Boxed<Value: Scored> { Held(Value) }");
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
    let declaration = only_enum(&result);
    let [param] = &declaration.type_params[..] else {
        panic!("expected one parameter, got {:?}", declaration.type_params);
    };
    assert_eq!(result.interner.resolve(param.name), "Value");
    let bounds: Vec<String> = param
        .bounds
        .iter()
        .map(|bound| result.interner.resolve(bound.name).to_owned())
        .collect();
    assert_eq!(bounds, ["Scored"]);
}

#[test]
fn several_bounds_on_one_parameter_are_joined_by_plus() {
    // The comma is taken: it separates parameters. The traits of one
    // parameter's bound are joined with `+`.
    let result = parse_text("enum Boxed<Value: Scored + Send, Rest> { Held(Value) Rest }");
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
    let declaration = only_enum(&result);
    assert_eq!(declaration.type_params.len(), 2);
    let bounds: Vec<String> = declaration.type_params[0]
        .bounds
        .iter()
        .map(|bound| result.interner.resolve(bound.name).to_owned())
        .collect();
    assert_eq!(bounds, ["Scored", "Send"]);
    // The unbounded second parameter still follows the bounded first.
    assert!(declaration.type_params[1].bounds.is_empty());
}

#[test]
fn a_bound_without_a_trait_name_is_reported() {
    let result = parse_text("enum Boxed<Value: > { Held(Value) }");
    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.has_code("KPAR079")),
        "{:?}",
        result.diagnostics,
    );
}
