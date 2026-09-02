use kira_syntax_model::ast::{Expr, Item, Stmt};

use super::{parse_text, type_spelling};

#[test]
fn parses_native_recover_type_argument_before_value_arguments() {
    let result = parse_text(
        "function f(raw: RawPtr) { var state = nativeRecover<CounterState>(raw) return }",
    );
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
    let Item::Function(function) = &result.tree.items()[0] else {
        panic!("expected a function");
    };
    let Stmt::Let { init, .. } = result.tree.stmt(function.body.stmts[0]) else {
        panic!("expected a binding");
    };
    let Expr::Call {
        callee,
        type_args,
        args,
        ..
    } = result.tree.expr(*init)
    else {
        panic!("expected a generic call");
    };
    assert_eq!(result.interner.resolve(*callee), "nativeRecover");
    assert_eq!(type_args.len(), 1);
    assert_eq!(type_spelling(&result, type_args[0]), "CounterState");
    assert_eq!(args.len(), 1);
}

#[test]
fn reports_an_empty_native_recover_type_argument_list() {
    let result = parse_text("function f(raw: RawPtr) { nativeRecover<>(raw) return }");
    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.has_code("KPAR046")),
        "{:?}",
        result.diagnostics,
    );
    assert_eq!(result.tree.items().len(), 1);
}

#[test]
fn reports_an_unclosed_native_recover_type_argument_list_without_bailing() {
    let result = parse_text("function f(raw: RawPtr) { nativeRecover<CounterState(raw) return }");
    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.has_code("KPAR045")),
        "{:?}",
        result.diagnostics,
    );
    assert_eq!(result.tree.items().len(), 1);
}
