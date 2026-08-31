//! Parser tests for a construction's content: the trailing block, the named
//! child fills that close it, and the positions where the same `name:` tokens
//! belong to something else.

use crate::*;
use kira_syntax_model::ast::{Expr, ExprId, Item};

fn parse_text(text: &str) -> ParseResult {
    parse(SourceId::new(0), text)
}

/// The initializer of the one `let` statement in the one function of `text`.
fn only_initializer(result: &ParseResult) -> ExprId {
    let [Item::Function(function)] = result.tree.items() else {
        panic!(
            "expected exactly one function, got {:?}",
            result.tree.items()
        );
    };
    match function.body.stmts.as_slice() {
        [statement] => match result.tree.stmt(*statement) {
            kira_syntax_model::ast::Stmt::Let { init, .. } => *init,
            other => panic!("expected a `let`, got {other:?}"),
        },
        stmts => panic!("expected exactly one statement, got {stmts:?}"),
    }
}

/// A construction's labels and children, for comparing a written form against
/// the shape it produces.
fn call_shape(result: &ParseResult, id: ExprId) -> (Vec<String>, usize) {
    let Expr::Call { args, children, .. } = result.tree.expr(id) else {
        panic!("expected a call, got {:?}", result.tree.expr(id));
    };
    let labels = args
        .iter()
        .map(|arg| {
            arg.label.map_or_else(
                || "_".to_owned(),
                |label| result.interner.resolve(label).to_owned(),
            )
        })
        .collect();
    (labels, children.len())
}

#[test]
fn a_named_fill_closes_a_construction_that_took_a_block() {
    let result = parse_text(
        r#"
function build() {
    let view = NavigationSplitView { Sidebar() } detail: { Content() }
}
"#,
    );
    let call = only_initializer(&result);
    assert_eq!(call_shape(&result, call), (vec!["detail".to_owned()], 1));
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
}

#[test]
fn a_named_fill_holds_a_content_block_rather_than_a_value() {
    let result = parse_text(
        r#"
function build() {
    let view = Split { Sidebar() } detail: { Header() Body() }
}
"#,
    );
    let call = only_initializer(&result);
    let Expr::Call { args, .. } = result.tree.expr(call) else {
        panic!("expected a call");
    };
    let Expr::Content { children, .. } = result.tree.expr(args[0].value) else {
        panic!(
            "expected a content block, got {:?}",
            result.tree.expr(args[0].value)
        );
    };
    assert_eq!(children.len(), 2);
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
}

#[test]
fn a_named_fill_may_carry_a_construction_or_a_plain_value() {
    let result = parse_text(
        r#"
function build() {
    let view = Split { Sidebar() } detail: Detail { Body() } tag: 7
}
"#,
    );
    let call = only_initializer(&result);
    assert_eq!(
        call_shape(&result, call),
        (vec!["detail".to_owned(), "tag".to_owned()], 1)
    );
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
}

/// A named fill written inside the block means what one written after it does:
/// both become a labeled argument of the same construction.
#[test]
fn a_fill_inside_the_block_and_one_after_it_agree() {
    let inside = parse_text(
        r#"
function build() {
    let view = Split { Sidebar() detail: Detail() }
}
"#,
    );
    let after = parse_text(
        r#"
function build() {
    let view = Split { Sidebar() } detail: Detail()
}
"#,
    );
    let inside_shape = call_shape(&inside, only_initializer(&inside));
    let after_shape = call_shape(&after, only_initializer(&after));
    assert_eq!(inside_shape, after_shape);
    assert_eq!(inside_shape, (vec!["detail".to_owned()], 1));
}

/// A construction that took no brace form accepts no named fill, so the
/// `spacing: 8` after a bare child stays the enclosing construction's override.
#[test]
fn an_override_after_a_bare_child_belongs_to_the_enclosing_construction() {
    let result = parse_text(
        r#"
function build() {
    let view = HStack { Text("a") spacing: 8 }
}
"#,
    );
    let call = only_initializer(&result);
    assert_eq!(call_shape(&result, call), (vec!["spacing".to_owned()], 1));
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
}

#[test]
fn a_dotted_method_call_inside_content_is_not_a_field_override() {
    let result = parse_text("function build() { let value = MainThread.invoke { window.read() } }");
    let call = only_initializer(&result);
    let Expr::MethodCall {
        children, method, ..
    } = result.tree.expr(call)
    else {
        panic!("expected a method call, got {:?}", result.tree.expr(call));
    };
    assert_eq!(result.interner.resolve(*method), "invoke");
    assert_eq!(children.len(), 1);
    assert!(matches!(
        result.tree.expr(children[0]),
        Expr::MethodCall { .. }
    ));
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
}

/// A struct literal's fields are separated by nothing, so a field name after a
/// braced value opens the next field rather than filling that value's slot.
#[test]
fn a_struct_literal_field_is_not_a_named_fill() {
    let result = parse_text(
        r#"
function build() {
    let style = Style { primary: Color { } secondary: Color { } }
}
"#,
    );
    let literal = only_initializer(&result);
    let Expr::StructLit { fields, .. } = result.tree.expr(literal) else {
        panic!(
            "expected a struct literal, got {:?}",
            result.tree.expr(literal)
        );
    };
    assert_eq!(fields.len(), 2);
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
}

/// A `?:` conditional's colon is not a fill binder: the `else` branch belongs
/// to the conditional, not to the construction the `then` branch built.
#[test]
fn a_conditional_else_branch_is_not_a_named_fill() {
    let result = parse_text(
        r#"
function build() {
    let view = wide ? Split { Sidebar() } : Stack { Sidebar() }
}
"#,
    );
    let value = only_initializer(&result);
    assert!(
        matches!(result.tree.expr(value), Expr::Conditional { .. }),
        "expected a conditional, got {:?}",
        result.tree.expr(value)
    );
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
}

/// The block a named fill follows may be empty and may sit after an argument
/// list: what admits the fill is that the construction closed with a `}`.
#[test]
fn an_empty_block_after_an_argument_list_still_admits_a_fill() {
    let result = parse_text(
        r#"
function build() {
    let view = Split(gap = 2) { } detail: Detail() sidebar: Sidebar()
}
"#,
    );
    let call = only_initializer(&result);
    assert_eq!(
        call_shape(&result, call),
        (
            vec!["gap".to_owned(), "detail".to_owned(), "sidebar".to_owned()],
            0
        )
    );
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
}
