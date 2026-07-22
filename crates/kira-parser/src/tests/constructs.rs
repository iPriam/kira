//! Parser tests for the construct declaration family: the `construct Family`
//! template and the construct-backed `Family Name(params)` declaration.

use crate::*;
use kira_syntax_model::ast::{ConstructKind, Item};

fn parse_text(text: &str) -> ParseResult {
    parse(SourceId::new(0), text)
}

/// The one construct declaration in `text`.
fn only_construct(result: &ParseResult) -> &kira_syntax_model::ast::ConstructDecl {
    match result.tree.items() {
        [Item::Construct(declaration)] => declaration,
        items => panic!("expected exactly one construct, got {items:?}"),
    }
}

#[test]
fn a_family_template_parses_with_a_required_field_and_a_computed_bridge() {
    let result = parse_text(
        r#"
construct Shape {
    @Required let sides: Int
    let area: Int { 0 }
}
"#,
    );
    let declaration = only_construct(&result);
    assert!(matches!(declaration.kind, ConstructKind::Family));
    assert_eq!(result.interner.resolve(declaration.name), "Shape");
    assert_eq!(declaration.fields.len(), 1);
    assert!(declaration.fields[0].required);
    assert_eq!(result.interner.resolve(declaration.fields[0].name), "sides");
    assert_eq!(declaration.methods.len(), 1);
    assert!(declaration.methods[0].computed);
    assert_eq!(
        result
            .interner
            .resolve(declaration.methods[0].function.name),
        "area"
    );
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
}

#[test]
fn a_backed_declaration_parses_its_family_params_and_computed_override() {
    let result = parse_text(
        r#"
Shape Square(side: Int) {
    let area: Int { side * side }
}
"#,
    );
    let declaration = only_construct(&result);
    let ConstructKind::Backed { family, params, .. } = &declaration.kind else {
        panic!("expected a backed declaration, got {:?}", declaration.kind);
    };
    assert_eq!(result.interner.resolve(*family), "Shape");
    assert_eq!(result.interner.resolve(declaration.name), "Square");
    assert_eq!(params.len(), 1);
    assert_eq!(result.interner.resolve(params[0].name), "side");
    assert_eq!(declaration.methods.len(), 1);
    assert!(declaration.methods[0].computed);
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
}

#[test]
fn a_backed_declaration_with_no_params_parses() {
    let result = parse_text(
        r#"
Widget Spacer() {
    let node: Int { 0 }
}
"#,
    );
    let declaration = only_construct(&result);
    let ConstructKind::Backed { params, .. } = &declaration.kind else {
        panic!("expected a backed declaration");
    };
    assert!(params.is_empty());
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
}

#[test]
fn a_function_member_parses_alongside_a_computed_member() {
    let result = parse_text(
        r#"
Widget Text(content: Int) {
    let node: Int { content }
    function width() -> Int {
        return content
    }
}
"#,
    );
    let declaration = only_construct(&result);
    assert_eq!(declaration.methods.len(), 2);
    assert!(declaration.methods[0].computed);
    assert!(!declaration.methods[1].computed);
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
}

#[test]
fn a_content_annotation_parses_as_a_real_child_slot_field() {
    let result = parse_text(
        r#"
Widget Stack() {
    @Content let children: [Int]
    let node: Int { 0 }
}
"#,
    );
    let declaration = only_construct(&result);
    // `@Content` is the compat spelling of a child slot: a real field, not a
    // deferred member.
    assert!(
        declaration.deferred.is_empty(),
        "{:?}",
        declaration.deferred
    );
    assert_eq!(declaration.fields.len(), 1);
    assert!(declaration.fields[0].slot);
    assert_eq!(
        result.interner.resolve(declaration.fields[0].name),
        "children"
    );
    // The executable computed member still parses beside it.
    assert_eq!(declaration.methods.len(), 1);
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
}

#[test]
fn a_some_slot_field_and_a_list_slot_field_parse() {
    let result = parse_text(
        r#"
Widget Wrap() {
    let inner: some Leaf
    let items: [some Leaf]
    let node: Int { 0 }
}
"#,
    );
    let declaration = only_construct(&result);
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
    assert_eq!(declaration.fields.len(), 2);
    assert!(declaration.fields[0].slot);
    assert_eq!(result.interner.resolve(declaration.fields[0].name), "inner");
    assert!(declaration.fields[1].slot);
    assert_eq!(result.interner.resolve(declaration.fields[1].name), "items");
    // The list slot's stored type is the array `[Leaf]`.
    assert!(matches!(
        result.tree.type_ref(declaration.fields[1].ty),
        kira_syntax_model::ast::TypeRef::Array { .. }
    ));
}

#[test]
fn a_body_shorthand_member_becomes_a_computed_family_typed_method() {
    let result = parse_text(
        r#"
Widget Divider() {
    body {
        Rectangle(width = 1.0)
    }
}
"#,
    );
    let declaration = only_construct(&result);
    assert!(declaration.deferred.is_empty());
    assert_eq!(declaration.methods.len(), 1);
    let method = &declaration.methods[0];
    assert!(method.computed);
    assert_eq!(result.interner.resolve(method.function.name), "body");
    let Some(return_type) = method.function.return_type else {
        panic!("body shorthand needs its family result type");
    };
    assert!(matches!(
        result.tree.type_ref(return_type),
        kira_syntax_model::ast::TypeRef::Named { name, .. }
            if result.interner.resolve(*name) == "Widget"
    ));
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
}

#[test]
fn a_consuming_function_member_parses_as_an_executable_method() {
    let result = parse_text(
        r#"
construct Widget {
    @Consuming
    function lower(context: Int) -> Int {
        return context
    }
}
"#,
    );
    let declaration = only_construct(&result);
    assert!(declaration.deferred.is_empty());
    assert_eq!(declaration.methods.len(), 1);
    assert!(!declaration.methods[0].computed);
    assert_eq!(
        result
            .interner
            .resolve(declaration.methods[0].function.name),
        "lower"
    );
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
}

#[test]
fn an_any_construct_type_keeps_its_family_qualifier() {
    let result = parse_text("struct Holder { let value: Any UI.Widget }");
    let [Item::Struct(declaration)] = result.tree.items() else {
        panic!("expected one struct");
    };
    assert!(matches!(
        result.tree.type_ref(declaration.fields[0].ty),
        kira_syntax_model::ast::TypeRef::AnyConstruct { family, .. }
            if result.interner.resolve(*family) == "UI.Widget"
    ));
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
}

#[test]
fn an_extends_clause_on_a_family_is_recorded_as_deferred() {
    let result = parse_text(
        r#"
construct Surface extends WebElement {
    let node: Int { 0 }
}
"#,
    );
    let declaration = only_construct(&result);
    assert_eq!(declaration.deferred.len(), 1);
    assert_eq!(declaration.deferred[0].label, "`extends` inheritance");
    assert_eq!(declaration.methods.len(), 1);
}
