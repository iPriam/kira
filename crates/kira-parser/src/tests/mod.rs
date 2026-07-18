//! Parser tests: the shapes the grammar must produce, and the recovery that
//! keeps one bad construct from derailing a file.
//!
//! Split out of `lib.rs` on the file-size ladder: the parser's own surface —
//! `parse`, `ParseResult`, and the token cursor — is ~250 lines, and the tests
//! had grown to twice that. They stay a `#[cfg(test)]` module of this crate,
//! beside the code they test.

mod aliases;
mod arrays;
mod classes;
mod closures;
mod enums;
mod exports;

use crate::*;
use kira_runtime_abi::Execution;
use kira_syntax_model::ast::{Expr, Function, Item, TypeRef, TypeRefId};
use kira_syntax_model::ownership::{OwnershipMode, OwnershipOp};

fn parse_text(text: &str) -> ParseResult {
    parse(SourceId::new(0), text)
}

/// The one function in `text`, for tests that parse a single declaration.
fn only_function(result: &ParseResult) -> &Function {
    match result.tree.items() {
        [Item::Function(function)] => function,
        items => panic!("expected exactly one function, got {items:?}"),
    }
}

/// Renders a written type back to its source spelling, so a test can assert
/// the *shape* the parser built rather than an arena index.
fn type_spelling(result: &ParseResult, id: TypeRefId) -> String {
    match result.tree.type_ref(id) {
        TypeRef::Named { name, .. } => result.interner.resolve(*name).to_owned(),
        TypeRef::Array { element, .. } => format!("[{}]", type_spelling(result, *element)),
        TypeRef::Function {
            params,
            result: ret,
            ..
        } => {
            let written: Vec<String> = params
                .iter()
                .map(|&param| type_spelling(result, param))
                .collect();
            format!(
                "({}) -> {}",
                written.join(", "),
                type_spelling(result, *ret)
            )
        }
        TypeRef::Error { .. } => "<error>".to_owned(),
    }
}

#[test]
fn execution_annotations_select_an_engine() {
    let runtime = parse_text("@Runtime function f() { return }");
    assert_eq!(only_function(&runtime).execution, Execution::Runtime);
    assert!(runtime.diagnostics.is_empty());

    let native = parse_text("@Native function f() { return }");
    assert_eq!(only_function(&native).execution, Execution::Native);
    assert!(native.diagnostics.is_empty());
}

#[test]
fn an_unannotated_function_inherits_the_builds_engine() {
    let plain = parse_text("function f() { return }");
    assert_eq!(only_function(&plain).execution, Execution::Inherited);
}

#[test]
fn execution_annotations_compose_with_main() {
    let result = parse_text("@Main @Native function main() { return }");
    let function = only_function(&result);
    assert!(function.is_main);
    assert_eq!(function.execution, Execution::Native);
    assert!(result.diagnostics.is_empty());
}

#[test]
fn two_engines_on_one_function_is_reported() {
    let result = parse_text("@Runtime @Native function f() { return }");
    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == Some("KPAR005")),
        "a contradictory engine pair must be reported, not silently resolved",
    );
    // Parsing still yields a usable function: the parser never bails.
    assert_eq!(result.tree.items().len(), 1);
}

#[test]
fn repeating_one_engine_is_not_a_contradiction() {
    let result = parse_text("@Native @Native function f() { return }");
    assert_eq!(only_function(&result).execution, Execution::Native);
    assert!(result.diagnostics.is_empty());
}

#[test]
fn parses_a_main_function() {
    let result = parse_text("@Main\nfunction main() { print(1) return }");
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
    assert_eq!(result.tree.items().len(), 1);
    match &result.tree.items()[0] {
        Item::Function(f) => {
            assert!(f.is_main);
            assert_eq!(result.interner.resolve(f.name), "main");
        }
        other => panic!("expected function, got {other:?}"),
    }
}

#[test]
fn parses_params_and_return_type() {
    let result = parse_text("function add(a: Int, b: Int) -> Int { return a + b }");
    match &result.tree.items()[0] {
        Item::Function(f) => {
            assert_eq!(f.params.len(), 2);
            assert!(f.return_type.is_some());
        }
        other => panic!("expected function, got {other:?}"),
    }
}

#[test]
fn colon_return_type_is_accepted() {
    let result = parse_text("function f(): Int { return 1 }");
    match &result.tree.items()[0] {
        Item::Function(f) => assert!(f.return_type.is_some()),
        other => panic!("expected function, got {other:?}"),
    }
}

#[test]
fn unsupported_constructs_do_not_crash() {
    // `construct` is still outside the subset (enums and classes now parse;
    // see `tests::enums` and `tests::classes`).
    let result = parse_text("construct C { }\n@Main function main() { return }");
    assert_eq!(result.tree.items().len(), 2);
    assert!(matches!(result.tree.items()[0], Item::Unsupported(_)));
    assert!(matches!(result.tree.items()[1], Item::Function(_)));
    assert!(result.diagnostics.iter().any(|d| d.code == Some("KSEM900")));
}

// ----- structs -------------------------------------------------------

/// The one struct in `text`, for tests that parse a single declaration.
fn only_struct(result: &ParseResult) -> &kira_syntax_model::ast::StructDecl {
    match result.tree.items() {
        [Item::Struct(declaration)] => declaration,
        items => panic!("expected exactly one struct, got {items:?}"),
    }
}

#[test]
fn parses_a_struct_with_let_and_var_members() {
    let result = parse_text("struct Point {\n    let x: Int\n    var y: Float\n}");
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
    let declaration = only_struct(&result);
    assert_eq!(result.interner.resolve(declaration.name), "Point");
    assert_eq!(declaration.fields.len(), 2);
    assert!(!declaration.fields[0].mutable);
    assert!(declaration.fields[1].mutable);
    assert!(declaration.fields[0].default.is_none());
}

#[test]
fn semicolons_separate_members_on_one_line() {
    let result = parse_text("struct Pair { var w: Int = 0; var h: Int = 0 }");
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
    let declaration = only_struct(&result);
    assert_eq!(declaration.fields.len(), 2);
    assert!(declaration.fields.iter().all(|f| f.default.is_some()));
}

#[test]
fn an_empty_struct_parses() {
    let result = parse_text("struct Blank {}");
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
    assert!(only_struct(&result).fields.is_empty());
}

#[test]
fn a_member_without_let_or_var_is_reported_and_recovers() {
    let result = parse_text("struct P { x: Int\n let y: Int }");
    assert!(result.diagnostics.iter().any(|d| d.code == Some("KPAR009")));
    // Recovery keeps the well-formed member: the parser never bails.
    let declaration = only_struct(&result);
    assert!(
        declaration
            .fields
            .iter()
            .any(|f| result.interner.resolve(f.name) == "y"),
    );
}

#[test]
fn methods_and_fields_interleave_in_a_struct_body() {
    let result =
        parse_text("struct P {\n let x: Int\n function sum() -> Int { return x }\n let y: Int\n}");
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
    let declaration = only_struct(&result);
    assert_eq!(declaration.fields.len(), 2, "{:?}", declaration.fields);
    assert_eq!(declaration.methods.len(), 1);
    assert_eq!(result.interner.resolve(declaration.methods[0].name), "sum");
}

#[test]
fn a_method_call_and_a_field_read_are_told_apart_by_the_parens() {
    let result = parse_text("function f() -> Int { return p.sum() + p.x }");
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
    let kira_syntax_model::ast::Stmt::Return {
        value: Some(id), ..
    } = first_stmt(&result)
    else {
        panic!("expected return");
    };
    let Expr::Binary { lhs, rhs, .. } = result.tree.expr(*id) else {
        panic!("expected a binary expression");
    };
    assert!(matches!(result.tree.expr(*lhs), Expr::MethodCall { .. }));
    assert!(matches!(result.tree.expr(*rhs), Expr::Field { .. }));
}

// ----- struct literals and field access ------------------------------

/// The first statement of the first function in `text`.
fn first_stmt(result: &ParseResult) -> &kira_syntax_model::ast::Stmt {
    let function = match &result.tree.items()[0] {
        Item::Function(function) => function,
        other => panic!("expected function, got {other:?}"),
    };
    result.tree.stmt(function.body.stmts[0])
}

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
        operator.tree.exprs.iter().any(|(_, expr)| matches!(
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
            .exprs
            .iter()
            .any(|(_, expr)| matches!(expr, Expr::Ownership { .. })),
        "`move + 1` reads a local named `move`, it does not transfer anything"
    );
}

/// `copy` behaves the same way, and both nest through unary operators.
#[test]
fn copy_parses_as_an_ownership_expression() {
    let result = parse_text("function f() { g(copy -1) return }");
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
    assert!(result.tree.exprs.iter().any(|(_, expr)| matches!(
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

// ----- imports ---------------------------------------------------------

#[test]
fn a_bare_import_parses_with_no_alias() {
    let result = parse(
        SourceId::new(0),
        "import support\n@Main function main() { return }",
    );
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
    match result.tree.items() {
        [Item::Import(declaration), Item::Function(_)] => {
            assert_eq!(declaration.path.len(), 1);
            assert_eq!(result.interner.resolve(declaration.path[0]), "support");
            assert!(declaration.alias.is_none());
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn an_aliased_import_records_its_alias() {
    let result = parse(
        SourceId::new(0),
        "import support as Support\n@Main function main() { return }",
    );
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
    match result.tree.items() {
        [Item::Import(declaration), ..] => {
            let alias = declaration.alias.expect("an alias was written");
            assert_eq!(result.interner.resolve(alias), "Support");
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_dotted_import_path_keeps_every_segment() {
    let result = parse(
        SourceId::new(0),
        "import Foundation.Web as Web\n@Main function main() { return }",
    );
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
    match result.tree.items() {
        [Item::Import(declaration), ..] => {
            let segments: Vec<&str> = declaration
                .path
                .iter()
                .map(|&segment| result.interner.resolve(segment))
                .collect();
            assert_eq!(segments, vec!["Foundation", "Web"]);
        }
        other => panic!("{other:?}"),
    }
}

/// Recovery: a malformed import must not derail the file. The import yields no
/// item — an import naming nothing would only produce a second, misleading
/// "unresolved module" — but the function after it still parses.
#[test]
fn a_malformed_import_still_leaves_the_rest_of_the_file_parsed() {
    let result = parse(
        SourceId::new(0),
        "import 42\n@Main function main() { print(1) return }",
    );
    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == Some("KPAR016")),
        "{:?}",
        result.diagnostics
    );
    assert!(
        result
            .tree
            .items()
            .iter()
            .any(|item| matches!(item, Item::Function(_))),
        "the function after the bad import still parses: {:?}",
        result.tree.items()
    );
}

/// `as` with no name after it is reported, and the import is still recorded —
/// the module is known, only the spelling of its root is not.
#[test]
fn an_alias_with_no_name_is_reported() {
    let result = parse(
        SourceId::new(0),
        "import support as\n@Main function main() { return }",
    );
    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == Some("KPAR017")),
        "{:?}",
        result.diagnostics
    );
}

/// A module-qualified type name is one `TypeRef::Named` whose symbol carries
/// the dot; a dot cannot appear in an identifier, so it can collide with
/// nothing a user declared.
#[test]
fn a_qualified_type_name_is_interned_with_its_qualifier() {
    let result = parse(SourceId::new(0), "function f(p: Support.Point) { return }");
    let function = match &result.tree.items()[0] {
        Item::Function(function) => function,
        other => panic!("{other:?}"),
    };
    match result.tree.type_ref(function.params[0].ty) {
        TypeRef::Named { name, .. } => {
            assert_eq!(result.interner.resolve(*name), "Support.Point");
        }
        other => panic!("{other:?}"),
    }
}
