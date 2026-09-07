//! Shape questions about a fragment of Kira source, answered by the real
//! parser.
//!
//! "Is this argument an expression?", "does this expansion parse as
//! statements?", and "is this a place?" all have exactly one right answer: the
//! one the compiler's own parser gives. Asking it here — by wrapping the
//! fragment in the smallest declaration that could hold it and parsing that —
//! is what keeps a macro diagnostic from disagreeing with the parse that
//! follows it.

use kira_diagnostics::has_errors;
use kira_source::SourceId;
use kira_syntax_model::ast::{Expr, Item, Stmt};

/// The source id every probe parse is attributed to.
///
/// Probes never surface a diagnostic — only whether there was one — so the id
/// is arbitrary and fixed rather than threaded through.
const PROBE: SourceId = SourceId::new(0);

/// Whether `text` is a single well-formed expression.
pub(crate) fn is_expression(text: &str) -> bool {
    single_statement(text).is_some_and(|stmt| matches!(stmt, Stmt::Expr { .. }))
}

/// Whether `text` is an assignable place: a name, a field path, or an index
/// into one.
pub(crate) fn is_place(text: &str) -> bool {
    let source = format!("function __kmac_probe() {{\n{text} = __kmac_probe_value\n}}\n");
    let parsed = kira_parser::parse(PROBE, &source);
    if has_errors(&parsed.diagnostics) {
        return false;
    }
    let Some(Item::Function(function)) = parsed.tree.items().first() else {
        return false;
    };
    let [only] = function.body.stmts.as_slice() else {
        return false;
    };
    let Stmt::Assign { target, .. } = parsed.tree.stmt(*only) else {
        return false;
    };
    is_place_expression(&parsed.tree, *target)
}

/// Whether the expression at `id` is a path that can be written through.
fn is_place_expression(
    tree: &kira_syntax_model::SyntaxTree,
    id: kira_syntax_model::ast::ExprId,
) -> bool {
    match tree.expr(id) {
        Expr::Name { .. } => true,
        Expr::Field { base, .. } => is_place_expression(tree, *base),
        Expr::Index { base, .. } => is_place_expression(tree, *base),
        _ => false,
    }
}

/// Whether `text` parses as a statement list inside a function body.
pub(crate) fn is_statements(text: &str) -> bool {
    let source = format!("function __kmac_probe() {{\n{text}\n}}\n");
    let parsed = kira_parser::parse(PROBE, &source);
    if has_errors(&parsed.diagnostics) {
        return false;
    }
    let Some(Item::Function(function)) = parsed.tree.items().first() else {
        return false;
    };
    !function
        .body
        .stmts
        .iter()
        .any(|stmt| matches!(parsed.tree.stmt(*stmt), Stmt::Error { .. }))
}

/// Whether `text` parses as a list of top-level declarations.
pub(crate) fn is_declarations(text: &str) -> bool {
    if text.trim().is_empty() {
        return true;
    }
    let parsed = kira_parser::parse(PROBE, text);
    !has_errors(&parsed.diagnostics)
        && !parsed
            .tree
            .items()
            .iter()
            .any(|item| matches!(item, Item::Unsupported(_)))
}

/// The one statement `text` parses to inside a function body, when it is
/// exactly one.
fn single_statement(text: &str) -> Option<Stmt> {
    let source = format!("function __kmac_probe() {{\n{text}\n}}\n");
    let parsed = kira_parser::parse(PROBE, &source);
    if has_errors(&parsed.diagnostics) {
        return None;
    }
    let Some(Item::Function(function)) = parsed.tree.items().first() else {
        return None;
    };
    let [only] = function.body.stmts.as_slice() else {
        return None;
    };
    Some(parsed.tree.stmt(*only).clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_expression_is_recognized() {
        assert!(is_expression("1 + 2"));
        assert!(is_expression("buildThing()"));
        assert!(is_expression("Point { x: 1 }"));
    }

    #[test]
    fn a_statement_is_not_an_expression() {
        assert!(!is_expression("let x = 1"));
        assert!(!is_expression("return 1"));
        assert!(!is_expression("let x = 1\nx"));
    }

    #[test]
    fn a_place_is_a_path() {
        assert!(is_place("left"));
        assert!(is_place("p.x"));
        assert!(is_place("xs[0].y"));
    }

    #[test]
    fn a_literal_is_not_a_place() {
        assert!(!is_place("1"));
        assert!(!is_place("f()"));
    }

    #[test]
    fn statements_and_declarations_are_told_apart() {
        assert!(is_statements("let x = 1\nx = 2"));
        assert!(is_declarations("function f() -> Int { return 1 }"));
        assert!(is_declarations(""));
        // A module-scope `let` is a declaration; an assignment never is.
        assert!(is_declarations("let x = 1"));
        assert!(!is_declarations("x = 2"));
    }
}
