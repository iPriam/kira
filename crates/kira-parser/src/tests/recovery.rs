//! Recovery limits: nesting depth caps, stray tokens, and the string rules
//! the lexer enforces. These tests pin the guarantee that pathological input
//! produces diagnostics, never a crash.

use kira_syntax_model::ast::{Expr, Stmt};

use super::{first_stmt, parse_text};

#[test]
fn deeply_nested_parens_are_refused_not_crashed() {
    let text = format!(
        "function f() {{ let x = {}0{} }}",
        "(".repeat(5_000),
        ")".repeat(5_000)
    );
    let result = parse_text(&text);
    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.has_code("KPAR068")),
        "expected a nesting diagnostic, got {:?}",
        result.diagnostics
    );
    // The refusal is reported once: the recovery consumes the refused group so
    // enclosing parens close against their own closers.
    assert_eq!(
        result
            .diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.has_code("KPAR068"))
            .count(),
        1
    );
}

#[test]
fn deeply_nested_unary_operators_are_refused_not_crashed() {
    let text = format!("function f() {{ let x = {}1 }}", "-".repeat(5_000));
    let result = parse_text(&text);
    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.has_code("KPAR068")),
        "{:?}",
        result.diagnostics
    );
}

#[test]
fn normal_nesting_still_parses_clean() {
    let text = format!(
        "function f() {{ let x = {}1{} }}",
        "(".repeat(100),
        ")".repeat(100)
    );
    let result = parse_text(&text);
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
}

#[test]
fn a_stray_identifier_after_a_name_is_refused_by_the_grammar() {
    // A second name where the grammar allows none is not swallowed: it is
    // refused at the next expectation (`(`, `=`, `{`, `in`).
    for text in [
        "function foo bar() {}",
        "let x y = 1",
        "struct S T {}",
        "enum E V { A }",
        "construct C D {}",
    ] {
        let result = parse_text(text);
        assert!(
            !result.diagnostics.is_empty(),
            "`{text}` parsed without refusing its stray identifier"
        );
    }
}

#[test]
fn return_try_carries_its_value() {
    let result = parse_text(
        // `handle` opens a BRACED group of arms — `} handle { Variant(x) { … } }`
        // — which is the spelling every `attempt` in tests-kik and the UI
        // packages uses. Written without them the arms have nowhere to sit.
        "function f() { attempt { return try g() } handle { MissingNode(reason) { return 0 } } }",
    );
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
    let Stmt::Attempt { body, .. } = first_stmt(&result) else {
        panic!("expected an attempt");
    };
    let Stmt::Return {
        value: Some(value), ..
    } = result.tree.stmt(body.stmts[0])
    else {
        panic!("expected `return` to open the attempt body");
    };
    assert!(
        matches!(result.tree.expr(*value), Expr::Try { .. }),
        "`return try g()` lost its value: {:?}",
        result.tree.expr(*value)
    );
}

#[test]
fn a_backslash_before_a_newline_continues_the_string() {
    // Pinned by tests-kik's StrxLiteralTests: the backslash-newline is a line
    // continuation, so the literal keeps going and nothing is unterminated.
    let result = parse_text("function f() {\n    let s = \"abc\\\ndef\"\n}");
    assert!(
        !result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.has_code("KLEX002")),
        "a continuation must not read as unterminated: {:?}",
        result.diagnostics
    );
}

#[test]
fn an_unknown_escape_is_reported() {
    let result = parse_text(r#"function f() { let s = "a\qb" }"#);
    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.has_code("KLEX003")),
        "{:?}",
        result.diagnostics
    );
}
