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

/// The `name { … }` shorthand becomes a computed method whose result type names
/// the *member of the family*, not the family.
///
/// The parser cannot answer what that member returns — it has no families — so
/// it records the question and semantics answers it against the declaration.
/// A family that declares `body` with a type gives the shorthand that type; one
/// that never mentions `body` falls back to the family type, which is what this
/// spelling used to mean unconditionally.
#[test]
fn a_body_shorthand_member_defers_its_result_type_to_the_family() {
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
        panic!("body shorthand needs a result type to defer");
    };
    assert!(matches!(
        result.tree.type_ref(return_type),
        kira_syntax_model::ast::TypeRef::ConstructMember { family, member, .. }
            if result.interner.resolve(*family) == "Widget"
                && result.interner.resolve(*member) == "body"
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
fn a_some_construct_type_keeps_its_family_qualifier() {
    let result = parse_text("struct Holder { let value: some UI.Widget }");
    let [Item::Struct(declaration)] = result.tree.items() else {
        panic!("expected one struct");
    };
    assert!(matches!(
        result.tree.type_ref(declaration.fields[0].ty),
        kira_syntax_model::ast::TypeRef::SomeConstruct { family, .. }
            if result.interner.resolve(*family) == "UI.Widget"
    ));
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
}

/// The pre-Construct-2.0 spelling parses to the same node, so checked-in Kira
/// that still writes `Any Widget` keeps compiling.
#[test]
fn the_compat_any_construct_spelling_parses_as_the_existential() {
    let result = parse_text("struct Holder { let value: Any UI.Widget }");
    let [Item::Struct(declaration)] = result.tree.items() else {
        panic!("expected one struct");
    };
    assert!(matches!(
        result.tree.type_ref(declaration.fields[0].ty),
        kira_syntax_model::ast::TypeRef::SomeConstruct { family, .. }
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

#[test]
fn an_extend_block_parses_its_modifier_functions() {
    let result = parse_text(
        r#"
extend Widget {
    function padding(length: Float) -> Widget {
        return Padding(length: length) {
            self
        }
    }

    function opacity(value: Float) -> Widget {
        return OpacityLayer(value: value) {
            self
        }
    }
}
"#,
    );
    let [Item::Extend(declaration)] = result.tree.items() else {
        panic!("expected one extend block, got {:?}", result.tree.items());
    };
    assert_eq!(result.interner.resolve(declaration.name), "Widget");
    assert_eq!(declaration.methods.len(), 2);
    assert_eq!(
        result.interner.resolve(declaration.methods[0].name),
        "padding"
    );
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
}

#[test]
fn extend_is_only_contextual_so_a_name_spelled_extend_is_unaffected() {
    // A construct-backed declaration whose *declaration* name is `extend` must
    // still parse as a backed declaration, not be mistaken for an extend block:
    // the block form is `extend Family {`, this is `Family extend {`.
    let result = parse_text("Widget extend {\n}\n");
    assert!(
        matches!(
            result.tree.items(),
            [Item::Construct(declaration)]
                if matches!(declaration.kind, ConstructKind::Backed { .. })
        ),
        "got {:?}",
        result.tree.items()
    );
}

/// `@Required function` is the Construct 2.0 spelling of a required behaviour:
/// a bodyless signature stored as a method member flagged `required`.
#[test]
fn a_family_template_parses_a_required_function_as_a_bodyless_member() {
    let result = parse_text(
        r#"
construct Shape {
    @Required function render(scale: Int) -> String
    @Required function reset()
}
"#,
    );
    let declaration = only_construct(&result);
    assert_eq!(declaration.methods.len(), 2);
    let render = &declaration.methods[0];
    assert!(render.required);
    assert!(!render.computed);
    assert_eq!(result.interner.resolve(render.function.name), "render");
    assert_eq!(render.function.params.len(), 1);
    assert!(render.function.return_type.is_some());
    assert!(render.function.body.stmts.is_empty());
    let reset = &declaration.methods[1];
    assert!(reset.required);
    assert!(reset.function.params.is_empty());
    // A requirement written without `-> T` carries no result type, which is what
    // semantics reads as "the family constrains the name and parameters only".
    assert!(reset.function.return_type.is_none());
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
}

/// `requires { … }` is the *section* spelling of `@Required function`, and it
/// produces exactly the same members — so a family may mix the two and nothing
/// downstream can tell which one was written.
#[test]
fn a_requires_section_parses_its_entries_as_required_members() {
    let result = parse_text(
        r#"
construct Shape {
    requires {
        function render(scale: Int) -> String
        function reset()
    }
    @Required function measure() -> Int
    let sides: Int = 3
}
"#,
    );
    let declaration = only_construct(&result);
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
    assert_eq!(declaration.methods.len(), 3);
    let names: Vec<&str> = declaration
        .methods
        .iter()
        .map(|method| result.interner.resolve(method.function.name))
        .collect();
    assert_eq!(names, ["render", "reset", "measure"]);
    for method in &declaration.methods {
        assert!(method.required, "{names:?}");
        assert!(!method.computed);
        assert!(method.function.body.stmts.is_empty());
    }
    assert_eq!(declaration.methods[0].function.params.len(), 1);
    assert!(declaration.methods[1].function.return_type.is_none());
    // The `let` after the section is still a member of the enclosing body, not
    // of the section: a section that swallowed the closing brace would lose it.
    assert_eq!(declaration.fields.len(), 1);
}

/// An empty `requires { }` is legal and states nothing, the way an empty body
/// does.
#[test]
fn an_empty_requires_section_is_accepted() {
    let result = parse_text("construct Shape {\n    requires { }\n}");
    let declaration = only_construct(&result);
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
    assert!(declaration.methods.is_empty());
}

/// A section lists signatures. Anything else is reported once and skipped, so
/// the entry after it still parses.
#[test]
fn a_requires_section_refuses_a_non_function_entry() {
    let result = parse_text(
        r#"
construct Shape {
    requires {
        let sides: Int
        function render() -> String
    }
}
"#,
    );
    assert!(
        result.diagnostics.iter().any(|d| d.has_code("KPAR066")),
        "{:?}",
        result.diagnostics
    );
    let declaration = only_construct(&result);
    assert_eq!(declaration.methods.len(), 1);
    assert!(declaration.methods[0].required);
}

/// A backed declaration parses the section the same way, which is what makes
/// one rule cover both: the members it produces are `@Required`, so semantics
/// refuses them there for the reason it already refuses that annotation
/// (`KSEM249`) rather than needing a second rule about sections.
#[test]
fn a_requires_section_on_a_backed_declaration_parses_as_requirements() {
    let result = parse_text(
        r#"
Shape Circle {
    requires {
        function render() -> String
    }
}
"#,
    );
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
    let declaration = only_construct(&result);
    assert_eq!(declaration.methods.len(), 1);
    assert!(declaration.methods[0].required);
}

/// A trailing `;` ends a requirement the way it ends a bodyless extern, and the
/// member after it still parses.
#[test]
fn a_required_function_may_end_with_a_semicolon() {
    let result = parse_text(
        r#"
construct Shape {
    @Required function render() -> String;
    @Required let sides: Int
}
"#,
    );
    let declaration = only_construct(&result);
    assert_eq!(declaration.methods.len(), 1);
    assert_eq!(declaration.fields.len(), 1);
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
}

/// A requirement states a signature; a body would make it an inheritable
/// default, which is the plain `function` member instead.
#[test]
fn a_required_function_with_a_body_is_refused() {
    let result = parse_text(
        r#"
construct Shape {
    @Required function render() -> String {
        return "x"
    }
}
"#,
    );
    assert!(
        result.diagnostics.iter().any(|d| d.has_code("KPAR065")),
        "{:?}",
        result.diagnostics
    );
}

/// `@Required` annotates a `let` or a `function`, and says so when it annotates
/// neither.
#[test]
fn a_required_annotation_on_neither_a_let_nor_a_function_is_refused() {
    let result = parse_text(
        r#"
construct Shape {
    @Required 7
}
"#,
    );
    assert!(
        result.diagnostics.iter().any(|d| d.has_code("KPAR060")),
        "{:?}",
        result.diagnostics
    );
}

#[test]
fn an_extend_block_refuses_a_non_function_member() {
    let result = parse_text(
        r#"
extend Widget {
    let stray: Int = 0
}
"#,
    );
    assert!(
        result.diagnostics.iter().any(|d| d.has_code("KPAR064")),
        "{:?}",
        result.diagnostics
    );
}

/// A modifier is a function, so the annotations that select how a function runs
/// reach it. They used to be a syntax error about a missing `function`.
#[test]
fn an_extend_modifier_carries_the_engine_its_annotation_selected() {
    let result = parse_text(
        r#"
extend Widget {
    @Native function padding(amount: Int) -> Widget {
        return self
    }
    function plain(amount: Int) -> Widget {
        return self
    }
}
"#,
    );
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
    let [Item::Extend(declaration)] = result.tree.items() else {
        panic!("expected one extend block: {:?}", result.tree.items());
    };
    assert_eq!(
        declaration.methods[0].execution,
        kira_runtime_abi::Execution::Native
    );
    assert_eq!(
        declaration.methods[1].execution,
        kira_runtime_abi::Execution::Inherited
    );
}

#[test]
fn an_annotation_in_an_extend_block_with_no_function_after_it_is_refused() {
    let result = parse_text(
        r#"
extend Widget {
    @Native let stray: Int = 0
}
"#,
    );
    assert!(
        result.diagnostics.iter().any(|d| d.has_code("KPAR064")),
        "{:?}",
        result.diagnostics
    );
}

/// The written type each single-item source puts in its one type position.
fn sole_written_type(result: &ParseResult) -> String {
    let id = match result.tree.items() {
        [Item::Function(function)] => function
            .params
            .first()
            .map(|param| param.ty)
            .or(function.return_type)
            .expect("a parameter or a return type"),
        [Item::Struct(declaration)] => declaration.fields[0].ty,
        [Item::Enum(declaration)] => declaration.variants[0].payload.expect("a payload"),
        [Item::TypeAlias(alias)] => alias.target,
        items => panic!("expected one item with one type, got {items:?}"),
    };
    super::type_spelling(result, id)
}

/// `some Family` is a type everywhere a type is written, not a spelling the
/// construct grammar owns: a parameter, a return type, an array element, a
/// struct field, and an enum payload all parse it into the same node.
#[test]
fn the_existential_parses_in_every_type_position() {
    let cases = [
        ("function f(w: some Widget) {}", "some Widget"),
        ("function f() -> some Widget { }", "some Widget"),
        ("struct Holder { let one: some Widget }", "some Widget"),
        ("struct Holder { let many: [some Widget] }", "[some Widget]"),
        ("enum Cell { Filled(some Widget) }", "some Widget"),
        ("type Alias = some Widget", "some Widget"),
        ("function f(w: [[some Widget]]) {}", "[[some Widget]]"),
        ("function f(w: some UI.Widget) {}", "some UI.Widget"),
    ];
    for (source, expected) in cases {
        let result = parse_text(source);
        assert!(
            result.diagnostics.is_empty(),
            "`{source}` did not parse: {:?}",
            result.diagnostics
        );
        assert_eq!(sole_written_type(&result), expected, "for `{source}`");
    }
}

/// `some` stays contextual: with no name after it, it is an ordinary type name,
/// so a parameter or binding called `some` parses as it always did.
#[test]
fn some_without_a_following_name_is_an_ordinary_identifier() {
    let result = parse_text("function f(some: Int) -> Int { return some }");
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
}

/// A local's type annotation may be the statement's last token, so the compat
/// `Any Family` spelling is not read there: `Any` is the top type, and the name
/// on the next line starts the next statement.
#[test]
fn a_bare_any_annotation_does_not_swallow_the_next_statement() {
    let result = parse_text(
        r#"
function f() -> Int {
    let a: Any = 1
    let b: Any = 2
    return 0
}
"#,
    );
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
}
