//! Parser tests for closures: the `{ params in body }` shape, the function
//! type, the trailing form, and what each recovers to.
//!
//! The whole grammar turns on one bounded lookahead — a `{` opens a closure
//! exactly when `in`, or a comma-separated run of identifiers then `in`,
//! follows. These pin both sides of that: what it accepts, and what it leaves
//! alone.

use kira_source::SourceId;
use kira_syntax_model::ast::{Expr, Item, Stmt};

use super::{parse_text, type_spelling};
use crate::parse;

/// The one function's body statements, for a test that parses one declaration.
fn only_body(result: &crate::ParseResult) -> Vec<Stmt> {
    let function = match &result.tree.items()[0] {
        Item::Function(function) => function,
        other => panic!("{other:?}"),
    };
    function
        .body
        .stmts
        .iter()
        .map(|&id| result.tree.stmt(id).clone())
        .collect()
}

#[test]
fn a_function_type_parses_in_a_parameter_and_a_result() {
    let result = parse_text("function f(g: (Int, Bool) -> String): (Int) -> Int { return g }");
    let function = match &result.tree.items()[0] {
        Item::Function(function) => function,
        other => panic!("{other:?}"),
    };
    assert_eq!(
        type_spelling(&result, function.params[0].ty),
        "(Int, Bool) -> String"
    );
    assert_eq!(
        type_spelling(&result, function.return_type.expect("a result type")),
        "(Int) -> Int"
    );
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
}

#[test]
fn a_zero_parameter_function_type_parses() {
    let result = parse_text("function f(g: () -> Void) { return }");
    let function = match &result.tree.items()[0] {
        Item::Function(function) => function,
        other => panic!("{other:?}"),
    };
    assert_eq!(type_spelling(&result, function.params[0].ty), "() -> Void");
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
}

#[test]
fn a_function_type_without_an_arrow_is_reported() {
    let result = parse_text("function f(g: (Int)) { return }");
    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == Some("KPAR034")),
        "{:?}",
        result.diagnostics
    );
}

#[test]
fn a_closure_literal_records_its_parameters() {
    let result = parse_text("function f() { let g = { a, b in return a } return }");
    let Stmt::Let { init, .. } = &only_body(&result)[0] else {
        panic!("expected a let");
    };
    match result.tree.expr(*init) {
        Expr::Closure { params, .. } => {
            let names: Vec<&str> = params
                .iter()
                .map(|param| result.interner.resolve(param.name))
                .collect();
            assert_eq!(names, ["a", "b"]);
        }
        other => panic!("{other:?}"),
    }
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
}

#[test]
fn a_bare_in_is_a_zero_parameter_closure() {
    let result = parse_text("function f() { let g = { in return 1 } return }");
    let Stmt::Let { init, .. } = &only_body(&result)[0] else {
        panic!("expected a let");
    };
    match result.tree.expr(*init) {
        Expr::Closure { params, body, .. } => {
            assert!(params.is_empty(), "`{{ in … }}` declares no parameters");
            assert_eq!(body.stmts.len(), 1);
        }
        other => panic!("{other:?}"),
    }
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
}

#[test]
fn a_struct_literal_is_still_a_struct_literal() {
    // `P { x = 1 }` is not `{ ident in`, so the lookahead leaves it alone.
    let result = parse_text("function f() { let p = P { x = 1 } return }");
    let Stmt::Let { init, .. } = &only_body(&result)[0] else {
        panic!("expected a let");
    };
    assert!(
        matches!(result.tree.expr(*init), Expr::StructLit { .. }),
        "{:?}",
        result.tree.expr(*init)
    );
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
}

#[test]
fn a_trailing_closure_becomes_the_last_argument() {
    let result = parse_text("function f() { register { v in return v } return }");
    let Stmt::Expr { expr, .. } = &only_body(&result)[0] else {
        panic!("expected an expression statement");
    };
    match result.tree.expr(*expr) {
        Expr::Call { callee, args, .. } => {
            assert_eq!(result.interner.resolve(*callee), "register");
            assert_eq!(args.len(), 1);
            assert!(matches!(result.tree.expr(args[0]), Expr::Closure { .. }));
        }
        other => panic!("{other:?}"),
    }
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
}

#[test]
fn a_trailing_closure_appends_to_an_existing_argument_list() {
    let result = parse_text("function f() { register(1, 2) { v in return v } return }");
    let Stmt::Expr { expr, .. } = &only_body(&result)[0] else {
        panic!("expected an expression statement");
    };
    match result.tree.expr(*expr) {
        Expr::Call { args, .. } => {
            assert_eq!(args.len(), 3);
            assert!(matches!(result.tree.expr(args[2]), Expr::Closure { .. }));
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_trailing_closure_on_a_field_becomes_a_method_call() {
    // `app.onEvent { … }` has no parentheses, so the field read is promoted to
    // a method call rather than left as a read followed by a stray block.
    let result = parse_text("function f() { app.onEvent { v in return v } return }");
    let Stmt::Expr { expr, .. } = &only_body(&result)[0] else {
        panic!("expected an expression statement");
    };
    match result.tree.expr(*expr) {
        Expr::MethodCall { method, args, .. } => {
            assert_eq!(result.interner.resolve(*method), "onEvent");
            assert_eq!(args.len(), 1);
            assert!(matches!(result.tree.expr(args[0]), Expr::Closure { .. }));
        }
        other => panic!("{other:?}"),
    }
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
}

#[test]
fn a_control_flow_body_is_never_a_trailing_closure() {
    // The `{` after a `while` condition opens the loop body. Nothing in it can
    // look like a closure header, and the gate on struct literals is what keeps
    // the question from even being asked.
    let result = parse_text("function f() { while ok { step() } return }");
    let Stmt::While { .. } = &only_body(&result)[0] else {
        panic!("expected a while, got {:?}", only_body(&result)[0]);
    };
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
}

#[test]
fn a_closure_returns_from_a_return_statement() {
    let result = parse_text("function f() { return { v in return v } }");
    let Stmt::Return { value, .. } = &only_body(&result)[0] else {
        panic!("expected a return");
    };
    let value = value.expect("the closure is the returned value");
    assert!(matches!(result.tree.expr(value), Expr::Closure { .. }));
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
}

#[test]
fn a_malformed_closure_parameter_recovers() {
    let result = parse(
        SourceId::new(0),
        "function f() { let g = { a, 1 in } return }",
    );
    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == Some("KPAR035")),
        "{:?}",
        result.diagnostics
    );
}
