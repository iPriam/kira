//! Enum parsing: declarations with the three variant shapes, leading-dot member
//! expressions, and the recovery a malformed variant falls back to.

use crate::*;
use kira_syntax_model::ast::{Expr, Item, Stmt};

use super::{parse_text, type_spelling};

/// The one enum declaration in `text`.
fn only_enum(result: &ParseResult) -> &kira_syntax_model::ast::EnumDecl {
    match result.tree.items.as_slice() {
        [Item::Enum(declaration)] => declaration,
        items => panic!("expected exactly one enum, got {items:?}"),
    }
}

#[test]
fn a_payload_less_enum_parses_its_variants() {
    let result = parse_text("enum Color { Red Green Blue }");
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
    let declaration = only_enum(&result);
    let names: Vec<String> = declaration
        .variants
        .iter()
        .map(|variant| result.interner.resolve(variant.name).to_owned())
        .collect();
    assert_eq!(names, ["Red", "Green", "Blue"]);
    assert!(declaration.variants.iter().all(|v| v.payload.is_none()));
}

#[test]
fn newlines_separate_variants_without_commas() {
    let result = parse_text("enum Color {\n  Red\n  Green\n  Blue\n}");
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
    assert_eq!(only_enum(&result).variants.len(), 3);
}

#[test]
fn a_paren_payload_and_a_colon_default_both_parse() {
    let result = parse_text("enum E {\n  Empty\n  Text(String)\n  Bad: String = \"oops\"\n}");
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
    let declaration = only_enum(&result);
    // Empty: no payload, no default.
    assert!(declaration.variants[0].payload.is_none());
    assert!(declaration.variants[0].default.is_none());
    // Text(String): a payload, no default.
    let text_ty = declaration.variants[1].payload.expect("a payload type");
    assert_eq!(type_spelling(&result, text_ty), "String");
    assert!(declaration.variants[1].default.is_none());
    // Bad: String = "oops": a payload and a default.
    let bad_ty = declaration.variants[2].payload.expect("a payload type");
    assert_eq!(type_spelling(&result, bad_ty), "String");
    assert!(declaration.variants[2].default.is_some());
}

#[test]
fn a_leading_dot_member_parses_with_and_without_a_payload() {
    // `.Red` (no parens) and `.Ok(12)` (a payload argument).
    let result = parse_text("@Main function main() { let a = .Red\n let b = .Ok(12) return }");
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
    let function = match &result.tree.items[0] {
        Item::Function(f) => f,
        other => panic!("expected a function, got {other:?}"),
    };
    let plain = match result.tree.stmt(function.body.stmts[0]) {
        Stmt::Let { init, .. } => *init,
        other => panic!("expected a let, got {other:?}"),
    };
    match result.tree.expr(plain) {
        Expr::DotMember { name, args, .. } => {
            assert_eq!(result.interner.resolve(*name), "Red");
            assert!(args.is_none(), "no parens means no argument list");
        }
        other => panic!("expected a dot member, got {other:?}"),
    }
    let with_payload = match result.tree.stmt(function.body.stmts[1]) {
        Stmt::Let { init, .. } => *init,
        other => panic!("expected a let, got {other:?}"),
    };
    match result.tree.expr(with_payload) {
        Expr::DotMember { name, args, .. } => {
            assert_eq!(result.interner.resolve(*name), "Ok");
            assert_eq!(args.as_ref().map(|a| a.len()), Some(1));
        }
        other => panic!("expected a dot member, got {other:?}"),
    }
}

#[test]
fn a_dot_member_is_a_comparison_operand() {
    // `c == .Red`: the leading dot sits where an expression does, and the `{`
    // of the enclosing `if` still ends the condition.
    let result = parse_text("@Main function main() { if c == .Red { print(1) } return }");
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
}

#[test]
fn a_malformed_variant_does_not_derail_the_file() {
    // A stray token where a variant name is expected is reported, and the
    // function after the enum still parses.
    let result = parse_text("enum C { 123 A }\n@Main function main() { return }");
    assert!(
        result.diagnostics.iter().any(|d| d.code == Some("KPAR031")),
        "{:?}",
        result.diagnostics
    );
    assert!(
        result
            .tree
            .items
            .iter()
            .any(|item| matches!(item, Item::Function(_))),
        "the function after a bad enum still parses"
    );
}
