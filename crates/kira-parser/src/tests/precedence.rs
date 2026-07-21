//! Operator-precedence tests: the fully-parenthesized shape the precedence
//! climber must build for each rung of the binary ladder and the conditional.
//!
//! Split out of `expr.rs` on the file-size ladder: the precedence table lives
//! there, but its tests are a cohesive block that reads better beside the
//! crate's other test modules than inline under the parser it exercises.

use crate::parse;
use kira_source::SourceId;
use kira_syntax_model::SyntaxTree;
use kira_syntax_model::ast::{BinaryOp, Expr, Item, Stmt};

/// Renders the first return-statement expression of the first function as
/// a fully-parenthesized string, to assert precedence shape.
fn return_shape(text: &str) -> String {
    let result = parse(SourceId::new(0), text);
    let tree = &result.tree;
    let function = match &tree.items()[0] {
        Item::Function(f) => f,
        other => panic!("expected function, got {other:?}"),
    };
    let stmt_id = *function.body.stmts.first().expect("a statement");
    let expr = match tree.stmt(stmt_id) {
        Stmt::Return {
            value: Some(expr), ..
        } => *expr,
        other => panic!("expected return with value, got {other:?}"),
    };
    render(tree, expr, &result.interner)
}

fn render(
    tree: &SyntaxTree,
    id: kira_syntax_model::ast::ExprId,
    interner: &kira_core::Interner,
) -> String {
    match tree.expr(id) {
        Expr::Int { value, .. } => value.to_string(),
        Expr::Bool { value, .. } => value.to_string(),
        Expr::Name { symbol, .. } => interner.resolve(*symbol).to_owned(),
        Expr::Unary { op, operand, .. } => {
            format!("({:?} {})", op, render(tree, *operand, interner))
        }
        Expr::Binary { op, lhs, rhs, .. } => format!(
            "({} {} {})",
            render(tree, *lhs, interner),
            spelling(*op),
            render(tree, *rhs, interner)
        ),
        Expr::Conditional {
            cond,
            then,
            otherwise,
            ..
        } => format!(
            "({} ? {} : {})",
            render(tree, *cond, interner),
            render(tree, *then, interner),
            render(tree, *otherwise, interner)
        ),
        other => format!("{other:?}"),
    }
}

fn spelling(op: BinaryOp) -> &'static str {
    op.spelling()
}

#[test]
fn multiplication_binds_tighter_than_addition() {
    assert_eq!(
        return_shape("function f() { return 2 + 3 * 4 }"),
        "(2 + (3 * 4))"
    );
}

#[test]
fn subtraction_is_left_associative() {
    assert_eq!(
        return_shape("function f() { return 10 - 2 - 3 }"),
        "((10 - 2) - 3)"
    );
}

#[test]
fn comparison_below_arithmetic_and_logic_below_comparison() {
    assert_eq!(
        return_shape("function f() { return 1 + 2 > 2 && 3 < 5 }"),
        "(((1 + 2) > 2) && (3 < 5))"
    );
}

#[test]
fn and_binds_tighter_than_or() {
    assert_eq!(
        return_shape("function f() { return true || false && false }"),
        "(true || (false && false))"
    );
}

#[test]
fn unary_binds_tighter_than_multiplication() {
    assert_eq!(
        return_shape("function f() { return 2 * -3 }"),
        "(2 * (Neg 3))"
    );
}

// The bitwise ladder. Each of these groups as C groups it, and as Go and
// Swift do not, so they pin the rungs a contributor is most likely to
// "correct" from memory.

#[test]
fn bitwise_and_binds_tighter_than_xor_and_or() {
    assert_eq!(
        return_shape("function f() { return 1 | 2 ^ 3 & 4 }"),
        "(1 | (2 ^ (3 & 4)))"
    );
}

#[test]
fn bitwise_or_binds_looser_than_equality() {
    // C reads this the same way; Go and Swift read it as `(1 | 2) == 3`.
    assert_eq!(
        return_shape("function f() { return 1 | 2 == 3 }"),
        "(1 | (2 == 3))"
    );
}

#[test]
fn bitwise_binds_tighter_than_logical_and() {
    assert_eq!(
        return_shape("function f() { return true && 1 | 2 }"),
        "(true && (1 | 2))"
    );
}

#[test]
fn shift_binds_tighter_than_comparison_and_looser_than_addition() {
    assert_eq!(
        return_shape("function f() { return 1 + 2 << 3 < 4 }"),
        "(((1 + 2) << 3) < 4)"
    );
}

#[test]
fn shift_is_left_associative() {
    assert_eq!(
        return_shape("function f() { return 1 << 2 >> 3 }"),
        "((1 << 2) >> 3)"
    );
}

#[test]
fn complement_binds_tighter_than_bitwise_and() {
    assert_eq!(
        return_shape("function f() { return ~1 & 2 }"),
        "((BitNot 1) & 2)"
    );
}

// The conditional.

#[test]
fn conditional_binds_looser_than_every_binary_operator() {
    assert_eq!(
        return_shape("function f() { return a || b ? 1 + 2 : 3 | 4 }"),
        "((a || b) ? (1 + 2) : (3 | 4))"
    );
}

#[test]
fn conditional_is_right_associative() {
    assert_eq!(
        return_shape("function f() { return a ? 1 : b ? 2 : 3 }"),
        "(a ? 1 : (b ? 2 : 3))"
    );
}

#[test]
fn conditional_nests_in_its_then_branch() {
    assert_eq!(
        return_shape("function f() { return a ? b ? 1 : 2 : 3 }"),
        "(a ? (b ? 1 : 2) : 3)"
    );
}

/// A conditional missing its `:` recovers rather than derailing the parse:
/// the error is reported and the function still yields one return
/// statement, so later items keep being parsed.
#[test]
fn conditional_without_colon_recovers() {
    let result = parse(SourceId::new(0), "function f() { return a ? 1 2 }");
    assert!(
        !result.diagnostics.is_empty(),
        "a missing `:` must be diagnosed"
    );
    let function = match &result.tree.items()[0] {
        Item::Function(f) => f,
        other => panic!("expected function, got {other:?}"),
    };
    assert!(
        !function.body.stmts.is_empty(),
        "the function body must survive the error"
    );
}
