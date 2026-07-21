//! Function, execution annotation, construct, and struct declaration parsing.

use kira_runtime_abi::Execution;
use kira_syntax_model::ast::{Expr, Item};

use super::{first_stmt, only_function, only_struct, parse_text};

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
    // A still-unsupported top-level form (`Package`) parses-don't-crash; enums,
    // classes, and constructs now parse (see `tests::enums`, `tests::classes`,
    // `tests::constructs`).
    let result = parse_text("Package { }\n@Main function main() { return }");
    assert_eq!(result.tree.items().len(), 2);
    assert!(matches!(result.tree.items()[0], Item::Unsupported(_)));
    assert!(matches!(result.tree.items()[1], Item::Function(_)));
    assert!(result.diagnostics.iter().any(|d| d.code == Some("KSEM900")));
}

#[test]
fn a_construct_now_parses_rather_than_reporting_unsupported() {
    let result = parse_text("construct C { }\n@Main function main() { return }");
    assert_eq!(result.tree.items().len(), 2);
    assert!(matches!(result.tree.items()[0], Item::Construct(_)));
    assert!(matches!(result.tree.items()[1], Item::Function(_)));
}

// ----- structs -------------------------------------------------------

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
