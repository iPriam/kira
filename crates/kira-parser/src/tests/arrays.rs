//! Array parsing: recursive `[Element]` types, literals with optional separators,
//! index postfix, and the recovery a malformed type falls back to.

use crate::*;
use kira_syntax_model::ast::Expr;
use kira_syntax_model::ownership::OwnershipMode;

use super::{first_stmt, only_function, parse_text, type_spelling};

/// Renders an expression's array shape, so a test asserts structure rather
/// than arena indices.
fn array_shape(result: &ParseResult, id: kira_syntax_model::ast::ExprId) -> String {
    match result.tree.expr(id) {
        Expr::Int { value, .. } => value.to_string(),
        Expr::Name { symbol, .. } => result.interner.resolve(*symbol).to_owned(),
        Expr::ArrayLit { elements, .. } => {
            let rendered: Vec<String> = elements
                .iter()
                .map(|&element| array_shape(result, element))
                .collect();
            format!("[{}]", rendered.join(" "))
        }
        Expr::Index { base, index, .. } => format!(
            "{}[{}]",
            array_shape(result, *base),
            array_shape(result, *index)
        ),
        Expr::Field { base, field, .. } => format!(
            "{}.{}",
            array_shape(result, *base),
            result.interner.resolve(*field)
        ),
        Expr::MethodCall {
            receiver,
            method,
            args,
            ..
        } => {
            let rendered: Vec<String> = args
                .iter()
                .map(|arg| array_shape(result, arg.value))
                .collect();
            format!(
                "{}.{}({})",
                array_shape(result, *receiver),
                result.interner.resolve(*method),
                rendered.join(" ")
            )
        }
        other => format!("{other:?}"),
    }
}

/// The expression of the first `return` in the first function.
fn returned(result: &ParseResult) -> kira_syntax_model::ast::ExprId {
    match first_stmt(result) {
        kira_syntax_model::ast::Stmt::Return {
            value: Some(id), ..
        } => *id,
        other => panic!("expected a return with a value, got {other:?}"),
    }
}

#[test]
fn an_array_type_nests_to_any_depth() {
    let result = parse_text("function f(a: [Int], b: [[Int]], c: [[[Point]]]) { return }");
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
    let params = &only_function(&result).params;
    assert_eq!(type_spelling(&result, params[0].ty), "[Int]");
    assert_eq!(type_spelling(&result, params[1].ty), "[[Int]]");
    assert_eq!(type_spelling(&result, params[2].ty), "[[[Point]]]");
}

#[test]
fn an_array_return_type_parses_in_both_spellings() {
    for text in [
        "function f() -> [Int] { return [] }",
        "function f(): [Int] { return [] }",
    ] {
        let result = parse_text(text);
        assert!(
            result.diagnostics.is_empty(),
            "{text}: {:?}",
            result.diagnostics
        );
        let ty = only_function(&result)
            .return_type
            .expect("a return type was written");
        assert_eq!(type_spelling(&result, ty), "[Int]");
    }
}

/// The ownership prefix is a contextual identifier recognized by what follows
/// it. `[` starts a type just as a name does, so a borrowed array parameter
/// must not read as a parameter whose type is named `borrow`.
#[test]
fn an_ownership_prefix_is_recognized_before_an_array_type() {
    let result =
        parse_text("function f(a: borrow [Int], b: move [[Int]], c: copy [Int]) { return }");
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
    let params = &only_function(&result).params;
    let modes: Vec<OwnershipMode> = params.iter().map(|param| param.ownership).collect();
    assert_eq!(
        modes,
        vec![
            OwnershipMode::BorrowRead,
            OwnershipMode::Move,
            OwnershipMode::Copy
        ]
    );
    // The prefix is stripped: what remains is the array type.
    assert_eq!(type_spelling(&result, params[0].ty), "[Int]");
    assert_eq!(type_spelling(&result, params[1].ty), "[[Int]]");
}

#[test]
fn borrow_mut_is_recognized_before_an_array_type() {
    let result = parse_text("function f(a: borrow mut [Int]) { return }");
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
    let params = &only_function(&result).params;
    assert_eq!(params[0].ownership, OwnershipMode::BorrowMut);
    assert_eq!(type_spelling(&result, params[0].ty), "[Int]");
}

/// Commas separate elements and a trailing one is fine; newlines are
/// whitespace, so a comma is what tells one element from the next.
#[test]
fn array_literal_commas_are_required_and_a_trailing_one_is_fine() {
    for text in [
        "function f() { return [1 2 3] }",
        "function f() { return [\n  1\n  2\n  3\n] }",
        "function f() { return [1, 2 3,] }",
    ] {
        let result = parse_text(text);
        assert!(
            result.diagnostics.iter().any(|d| d.has_code("KPAR002")),
            "{text}: {:?}",
            result.diagnostics
        );
    }
    for text in [
        "function f() { return [1, 2, 3] }",
        "function f() { return [1, 2, 3,] }",
        "function f() { return [\n  1,\n  2,\n  3,\n] }",
    ] {
        let result = parse_text(text);
        assert!(
            result.diagnostics.is_empty(),
            "{text}: {:?}",
            result.diagnostics
        );
        assert_eq!(array_shape(&result, returned(&result)), "[1 2 3]", "{text}");
    }
}

#[test]
fn an_empty_array_literal_parses() {
    let result = parse_text("function f() { return [] }");
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
    assert_eq!(array_shape(&result, returned(&result)), "[]");
}

#[test]
fn array_literals_nest() {
    let result = parse_text("function f() { return [[1, 2], [3]] }");
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
    assert_eq!(array_shape(&result, returned(&result)), "[[1 2] [3]]");
}

/// Index and field postfixes chain through one loop, so a path of any shape
/// parses left-associatively.
#[test]
fn index_and_field_postfixes_chain_in_either_order() {
    let cases = [
        ("function f() { return xs[0] }", "xs[0]"),
        ("function f() { return grid[0][1] }", "grid[0][1]"),
        (
            "function f() { return grid[0].cells[2].x }",
            "grid[0].cells[2].x",
        ),
        (
            "function f() { return holder.values[3] }",
            "holder.values[3]",
        ),
        ("function f() { return xs.count }", "xs.count"),
        (
            "function f() { return rows[0].xs.count }",
            "rows[0].xs.count",
        ),
    ];
    for (text, expected) in cases {
        let result = parse_text(text);
        assert!(
            result.diagnostics.is_empty(),
            "{text}: {:?}",
            result.diagnostics
        );
        assert_eq!(array_shape(&result, returned(&result)), expected, "{text}");
    }
}

#[test]
fn append_parses_as_a_method_call_on_a_path() {
    let result = parse_text("function f() { return rows[0].xs.append(42) }");
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
    assert_eq!(
        array_shape(&result, returned(&result)),
        "rows[0].xs.append(42)"
    );
}

/// An index is a value position, so a struct literal inside one is legal even
/// where a bare `{` would otherwise end the expression.
#[test]
fn a_struct_literal_is_legal_inside_an_index() {
    let result = parse_text("function f() { while xs[at(P { n = 1 })] < 3 { break } }");
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
}

/// A `for` header sits under `without_struct_literals`, so the loop body's `{`
/// ends the iterable rather than opening a literal — an array iterable must
/// not break that.
#[test]
fn for_over_an_array_keeps_its_body_brace() {
    let result = parse_text("function f() { for x in xs { print(x) } }");
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
}

#[test]
fn an_unclosed_array_literal_is_reported_and_recovers() {
    let result = parse_text("function f() { let a = [1, 2\n}\nfunction g() { return 1 }");
    assert!(
        !result.diagnostics.is_empty(),
        "an unclosed `[` must be reported"
    );
    // Recovery keeps the following declaration: the parser never bails.
    assert_eq!(result.tree.items().len(), 2, "{:?}", result.tree.items());
}

#[test]
fn a_malformed_type_recovers_to_an_error_node_the_parser_reported() {
    let result = parse_text("function f(a: [) { return }");
    assert!(result.diagnostics.iter().any(|d| d.has_code("KPAR006")));
    let params = &only_function(&result).params;
    assert_eq!(type_spelling(&result, params[0].ty), "[<error>]");
}
