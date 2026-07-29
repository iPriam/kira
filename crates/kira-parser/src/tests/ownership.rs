//! Recovery, newline continuation, and ownership syntax parsing.

use crate::*;
use kira_syntax_model::ast::{Expr, Function, Item};
use kira_syntax_model::ownership::{OwnershipMode, OwnershipOp};

use super::{only_function, parse_text, type_spelling};

#[test]
fn import_then_function_recovers() {
    let result = parse_text("import Foundation\n@Main function main() { return }");
    assert_eq!(result.tree.items().len(), 2);
    assert!(matches!(result.tree.items()[1], Item::Function(_)));
}

#[test]
fn missing_brace_still_terminates() {
    let result = parse_text("function f() { return 1");
    assert!(!result.diagnostics.is_empty());
    assert_eq!(result.tree.items().len(), 1);
}

// ----- newline-boundary parity (verified against the reference) -------
//
// The reference implementation treats newlines as insignificant inside a
// function body: expressions continue across lines and `return` attaches a
// next-line value. Verified by running the reference on scratch packages:
//   `let a = 5` / `-2` / `print(a)`  -> prints 3   (continuation)
//   `return` / `42` in an Int fn     -> returns 42 (value attaches)
//   `return` / `print(..)` in a Void fn -> rejected (value attaches, Void
//    functions cannot return one)
// These tests lock that parity so a future "newline ends the statement"
// change cannot land silently.

fn function_body(result: &ParseResult) -> &Function {
    match &result.tree.items()[0] {
        Item::Function(function) => function,
        other => panic!("expected function, got {other:?}"),
    }
}

#[test]
fn binary_expression_continues_across_a_newline() {
    let result = parse_text("function f() -> Int {\n    let a = 5\n    -2\n    return a\n}");
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
    let function = function_body(&result);
    // `5` and `-2` fold into one initializer: exactly two statements.
    assert_eq!(function.body.stmts.len(), 2);
    let kira_syntax_model::ast::Stmt::Let { init, .. } = result.tree.stmt(function.body.stmts[0])
    else {
        panic!("expected let");
    };
    assert!(matches!(
        result.tree.expr(*init),
        Expr::Binary {
            op: kira_syntax_model::ast::BinaryOp::Sub,
            ..
        }
    ));
}

#[test]
fn return_attaches_a_next_line_value() {
    let result = parse_text("function f() -> Int {\n    return\n    42\n}");
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
    let function = function_body(&result);
    assert_eq!(function.body.stmts.len(), 1);
    assert!(matches!(
        result.tree.stmt(function.body.stmts[0]),
        kira_syntax_model::ast::Stmt::Return { value: Some(_), .. }
    ));
}

// ----- ownership syntax ---------------------------------------------

/// Every parameter mode parses, and a bare type is `Owned` rather than a
/// missing value.
#[test]
fn parameter_ownership_modes_parse() {
    let result = parse_text(
        "function f(a: Int, b: borrow Int, c: borrow mut Int, d: move Int, e: copy Int) { return }",
    );
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
    let modes: Vec<OwnershipMode> = only_function(&result)
        .params
        .iter()
        .map(|param| param.ownership)
        .collect();
    assert_eq!(
        modes,
        vec![
            OwnershipMode::Owned,
            OwnershipMode::BorrowRead,
            OwnershipMode::BorrowMut,
            OwnershipMode::Move,
            OwnershipMode::Copy,
        ]
    );
    // The prefix is stripped: the type is what remains.
    for param in &only_function(&result).params {
        assert_eq!(type_spelling(&result, param.ty), "Int");
    }
}

/// `borrow`, `move`, and `copy` are contextual identifiers. A parameter
/// *named* one of them still parses as a name, because the mode is only
/// recognized when a type follows it.
#[test]
fn ownership_words_are_still_usable_as_parameter_names() {
    let result = parse_text("function f(borrow: Int, move: Int, copy: Int, mut: Int) { return }");
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
    let function = only_function(&result);
    assert_eq!(function.params.len(), 4);
    for param in &function.params {
        assert_eq!(param.ownership, OwnershipMode::Owned);
        assert_eq!(type_spelling(&result, param.ty), "Int");
    }
    assert_eq!(result.interner.resolve(function.params[0].name), "borrow");
}

/// `move x` is an ownership expression; `move` alone is a name. The
/// lookahead is the only thing separating them.
#[test]
fn move_is_an_operator_only_when_an_operand_follows() {
    let operator = parse_text("function f() { g(move x) return }");
    assert!(
        operator.diagnostics.is_empty(),
        "{:?}",
        operator.diagnostics
    );
    assert!(
        operator.tree.exprs().any(|(_, expr)| matches!(
            expr,
            Expr::Ownership {
                op: OwnershipOp::Move,
                ..
            }
        )),
        "`move x` parses as an ownership expression"
    );

    let name = parse_text("function f() -> Int { let move = 1 return move + 1 }");
    assert!(name.diagnostics.is_empty(), "{:?}", name.diagnostics);
    assert!(
        !name
            .tree
            .exprs()
            .any(|(_, expr)| matches!(expr, Expr::Ownership { .. })),
        "`move + 1` reads a local named `move`, it does not transfer anything"
    );
}

/// `copy` behaves the same way, and both nest through unary operators.
#[test]
fn copy_parses_as_an_ownership_expression() {
    let result = parse_text("function f() { g(copy -1) return }");
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
    assert!(result.tree.exprs().any(|(_, expr)| matches!(
        expr,
        Expr::Ownership {
            op: OwnershipOp::Copy,
            ..
        }
    )));
}

/// A `move` with nothing to move is a name read, so it recovers as an
/// undefined-name problem later rather than derailing the parse here.
#[test]
fn a_dangling_move_does_not_derail_the_parse() {
    let result = parse_text("function f() { let a = move\n return }");
    assert!(
        result
            .tree
            .items()
            .iter()
            .any(|item| matches!(item, Item::Function(_))),
        "the function still parses"
    );
}

#[test]
fn parenthesized_expression_spans_lines() {
    let result = parse_text(
        "function f() -> Int {\n    let a = (1 +\n        2 +\n        3)\n    return a\n}",
    );
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
    let function = function_body(&result);
    assert_eq!(function.body.stmts.len(), 2);
}
