//! Parser shapes for inferred construct members, bare braced construction, and
//! dotted copy/update overrides.

use crate::*;
use kira_syntax_model::ast::{Expr, Item, Stmt};

fn parse_text(text: &str) -> ParseResult {
    parse(SourceId::new(0), text)
}

#[test]
fn a_construct_member_may_omit_its_type_when_it_has_an_initializer() {
    let result = parse_text(
        r#"
construct Style {
    let opacity = 1.0
}
"#,
    );
    let [Item::Construct(declaration)] = result.tree.items() else {
        panic!("expected one construct declaration");
    };
    assert!(declaration.fields[0].ty.is_none());
    assert!(declaration.fields[0].default.is_some());
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
}

#[test]
fn a_construct_var_member_is_recorded_as_mutable_storage() {
    let result = parse_text(
        r#"
construct Panel() extends Widget {
    var count: Int = 0
}
"#,
    );
    let [Item::Construct(declaration)] = result.tree.items() else {
        panic!("expected one construct declaration");
    };
    assert!(declaration.fields[0].mutable);
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
}

#[test]
fn bare_braces_and_dotted_let_overrides_remain_a_braced_call() {
    let result = parse_text(
        r#"
function main() {
    let base = Style {}
    let copy = base {
        let liquidGlass.material = .XHigh
    }
}
"#,
    );
    let [Item::Function(function)] = result.tree.items() else {
        panic!("expected one function");
    };
    let Stmt::Let { init, .. } = result.tree.stmt(function.body.stmts[1]) else {
        panic!("expected the copy binding");
    };
    let Expr::Call { braced, args, .. } = result.tree.expr(*init) else {
        panic!("expected a braced call");
    };
    assert!(*braced);
    assert_eq!(args.len(), 1);
    assert_eq!(
        result
            .interner
            .resolve(args[0].label.expect("override label")),
        "liquidGlass.material"
    );
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
}

#[test]
fn bare_content_braces_keep_the_existing_child_shape() {
    let result = parse_text(
        r#"
function main() {
    let value = Stack { Text("child") }
}
"#,
    );
    let [Item::Function(function)] = result.tree.items() else {
        panic!("expected one function");
    };
    let Stmt::Let { init, .. } = result.tree.stmt(function.body.stmts[0]) else {
        panic!("expected a binding");
    };
    let Expr::Call {
        braced,
        children,
        args,
        ..
    } = result.tree.expr(*init)
    else {
        panic!("expected a content call");
    };
    assert!(*braced);
    assert!(args.is_empty());
    assert_eq!(children.len(), 1);
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
}
