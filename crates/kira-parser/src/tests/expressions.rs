//! Struct literals, labeled calls, field access, and assignment parsing.

use crate::ParseResult;
use kira_syntax_model::ast::{Expr, Item};

use super::{first_stmt, parse_text};

// ----- integer literals ----------------------------------------------

/// The value of the single `let` initializer in `text`.
fn let_int(text: &str) -> i64 {
    let result = parse_text(text);
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
    let kira_syntax_model::ast::Stmt::Let { init, .. } = first_stmt(&result) else {
        panic!("expected let");
    };
    let Expr::Int { value, .. } = result.tree.expr(*init) else {
        panic!(
            "expected an integer literal, got {:?}",
            result.tree.expr(*init)
        );
    };
    *value
}

#[test]
fn a_hex_literal_carries_its_value() {
    assert_eq!(let_int("function f() { let n = 0xff }"), 255);
    assert_eq!(let_int("function f() { let n = 0x1bc6ea02 }"), 0x1bc6_ea02);
    assert_eq!(let_int("function f() { let n = 0Xdead }"), 0xdead);
}

#[test]
fn a_hex_literal_is_a_bit_pattern_and_a_decimal_one_is_a_number() {
    // Sixty-four bits set is `-1` written as C writes a mask, not an overflow.
    assert_eq!(let_int("function f() { let n = 0xffffffffffffffff }"), -1);
    // A decimal literal keeps its own range, whichever way the same value could
    // have been spelled.
    let refused = parse_text("function f() { let n = 18446744073709551615 }");
    assert!(
        refused
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == Some("KPAR021")),
        "{:?}",
        refused.diagnostics
    );
    // And a hex literal past sixty-four bits is refused the same way.
    let too_wide = parse_text("function f() { let n = 0x1ffffffffffffffff }");
    assert!(
        too_wide
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == Some("KPAR021")),
        "{:?}",
        too_wide.diagnostics
    );
}

#[test]
fn a_hex_literal_is_an_ordinary_argument_inside_a_struct_literal() {
    // The shape the corpus writes: a newline-separated literal whose field value
    // is a call taking several hex arguments.
    let result = parse_text(
        "function f() { let e = Entry {
         iid: guid(0x1bc6ea02, 0xef36, 0x464f)
         tag: 3
         } }",
    );
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
}

// ----- struct literals and field access ------------------------------

#[test]
fn parses_a_struct_literal_with_both_binders() {
    let result = parse_text("function f() { let p = Point { x = 1, y: 2 } }");
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
    let kira_syntax_model::ast::Stmt::Let { init, .. } = first_stmt(&result) else {
        panic!("expected let");
    };
    let Expr::StructLit { fields, .. } = result.tree.expr(*init) else {
        panic!("expected a struct literal");
    };
    assert_eq!(fields.len(), 2, "both binders normalize to one node");
}

#[test]
fn a_module_qualified_struct_literal_parses() {
    // `Support.Point { … }` parses as one struct literal whose name keeps the
    // dotted qualifier — semantics strips it against the file's imports — rather
    // than a field read `Support.Point` followed by an unparsable `{`.
    let result = parse_text("function f() { let p = Support.Point { x = 1, y: 2 } }");
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
    let kira_syntax_model::ast::Stmt::Let { init, .. } = first_stmt(&result) else {
        panic!("expected let");
    };
    let Expr::StructLit { name, fields, .. } = result.tree.expr(*init) else {
        panic!(
            "expected a struct literal, got {:?}",
            result.tree.expr(*init)
        );
    };
    assert_eq!(result.interner.resolve(*name), "Support.Point");
    assert_eq!(fields.len(), 2);
}

#[test]
fn a_deeper_qualified_struct_literal_keeps_every_segment() {
    let result = parse_text("function f() { let v = A.B.Thing { n = 1 } }");
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
    let kira_syntax_model::ast::Stmt::Let { init, .. } = first_stmt(&result) else {
        panic!("expected let");
    };
    let Expr::StructLit { name, .. } = result.tree.expr(*init) else {
        panic!("expected a struct literal");
    };
    assert_eq!(result.interner.resolve(*name), "A.B.Thing");
}

#[test]
fn a_qualified_call_parses_as_a_method_call() {
    // `Support.hello(1)` is a method call at the parser level: the receiver is
    // the bare name `Support`, and only the analyzer decides it is a module.
    let result = parse_text("function f() { let v = Support.hello(1) }");
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
    let kira_syntax_model::ast::Stmt::Let { init, .. } = first_stmt(&result) else {
        panic!("expected let");
    };
    let Expr::MethodCall { receiver, .. } = result.tree.expr(*init) else {
        panic!("expected a method call, got {:?}", result.tree.expr(*init));
    };
    assert!(matches!(result.tree.expr(*receiver), Expr::Name { .. }));
}

/// A `{` in a condition still opens the block, never a qualified literal — the
/// same rule a bare `Name { … }` follows, so `Module.Type` there is a field
/// read and the loop body is the block.
#[test]
fn a_qualified_name_before_a_condition_brace_opens_a_block() {
    let result = parse_text("function f() { if flags.ready { return } }");
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
    let kira_syntax_model::ast::Stmt::If { cond, .. } = first_stmt(&result) else {
        panic!("expected an if, got {:?}", first_stmt(&result));
    };
    assert!(
        matches!(result.tree.expr(*cond), Expr::Field { .. }),
        "the condition is a field read, not a struct literal",
    );
}

/// The call-argument analogue of the struct-literal binder: `f(label: v)` and
/// `f(label = v)` both record the label, and a bare argument records none.
fn call_args(result: &ParseResult) -> Vec<kira_syntax_model::ast::CallArg> {
    let kira_syntax_model::ast::Stmt::Let { init, .. } = first_stmt(result) else {
        panic!("expected let");
    };
    match result.tree.expr(*init) {
        Expr::Call { args, .. } => args.clone(),
        other => panic!("expected a call, got {other:?}"),
    }
}

#[test]
fn a_call_records_argument_labels_with_either_binder() {
    let result = parse_text("function f() { let v = measure(tree: a, index = b, c) }");
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
    let args = call_args(&result);
    assert_eq!(args.len(), 3);
    assert_eq!(
        result
            .interner
            .resolve(args[0].label.expect("first is labeled")),
        "tree"
    );
    assert_eq!(
        result
            .interner
            .resolve(args[1].label.expect("second is labeled")),
        "index"
    );
    assert!(args[2].label.is_none(), "a bare argument carries no label");
}

#[test]
fn a_bare_identifier_argument_is_not_a_label() {
    // `f(x)` is a positional argument, not a label: the binder is what makes a
    // leading identifier a label, and there is none here.
    let result = parse_text("function f() { let v = g(x) }");
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
    let args = call_args(&result);
    assert_eq!(args.len(), 1);
    assert!(args[0].label.is_none());
}

#[test]
fn an_enum_payload_rejects_an_argument_label() {
    // A variant payload binds by shape, not by name, so a label there is a
    // parse error rather than a binder.
    let result = parse_text("function f() { let v = .Ok(value: 1) }");
    let codes: Vec<_> = result
        .diagnostics
        .iter()
        .filter_map(|diagnostic| diagnostic.code)
        .collect();
    assert_eq!(codes, vec!["KPAR056"]);
}

#[test]
fn struct_literal_fields_need_no_separator() {
    // Newlines are insignificant, so a comma cannot be required.
    let result = parse_text("function f() { let p = Point {\n    x = 1\n    y = 2\n} }");
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
    let kira_syntax_model::ast::Stmt::Let { init, .. } = first_stmt(&result) else {
        panic!("expected let");
    };
    let Expr::StructLit { fields, .. } = result.tree.expr(*init) else {
        panic!("expected a struct literal");
    };
    assert_eq!(fields.len(), 2);
}

#[test]
fn a_brace_after_a_condition_opens_a_block_not_a_literal() {
    // The ambiguity newline-insignificance creates: `if flag { … }` must
    // read as a condition plus a block, never as a literal `flag { … }`.
    let result = parse_text("function f() { if flag { return } }");
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
    let kira_syntax_model::ast::Stmt::If {
        cond, then_block, ..
    } = first_stmt(&result)
    else {
        panic!("expected if");
    };
    assert!(matches!(result.tree.expr(*cond), Expr::Name { .. }));
    assert_eq!(then_block.stmts.len(), 1);
}

#[test]
fn a_parenthesized_literal_is_still_allowed_in_a_condition() {
    let result = parse_text("function f() { if (Point { x = 1 }).x > 0 { return } }");
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
    assert!(matches!(
        first_stmt(&result),
        kira_syntax_model::ast::Stmt::If { .. }
    ));
}

#[test]
fn a_literal_inside_a_condition_call_is_allowed() {
    // The suppression must not leak past a delimiter.
    let result = parse_text("function f() { while check(Point { x = 1 }) { return } }");
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
    assert!(matches!(
        first_stmt(&result),
        kira_syntax_model::ast::Stmt::While { .. }
    ));
}

#[test]
fn parses_a_chained_field_read() {
    let result = parse_text("function f() -> Int { return b.size.x }");
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
    let kira_syntax_model::ast::Stmt::Return {
        value: Some(id), ..
    } = first_stmt(&result)
    else {
        panic!("expected return");
    };
    let Expr::Field { base, .. } = result.tree.expr(*id) else {
        panic!("expected a field read");
    };
    // Left-associative: `(b.size).x`.
    assert!(matches!(result.tree.expr(*base), Expr::Field { .. }));
}

#[test]
fn parses_assignment_to_a_local_and_to_a_field_path() {
    let result = parse_text("function f() { x = 1\n b.size.x = 2 }");
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
    let function = match &result.tree.items()[0] {
        Item::Function(function) => function,
        other => panic!("expected function, got {other:?}"),
    };
    assert_eq!(function.body.stmts.len(), 2);
    for stmt in &function.body.stmts {
        assert!(matches!(
            result.tree.stmt(*stmt),
            kira_syntax_model::ast::Stmt::Assign { .. }
        ));
    }
}
