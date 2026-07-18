//! Class parsing: the `extends` list, `override` members, and what a malformed
//! class body recovers to.

use crate::*;
use kira_syntax_model::ast::Item;

use super::parse_text;

/// The one class declaration in `text`.
fn only_class(result: &ParseResult) -> &kira_syntax_model::ast::ClassDecl {
    match result.tree.items() {
        [Item::Class(declaration)] => declaration,
        items => panic!("expected exactly one class, got {items:?}"),
    }
}

#[test]
fn a_class_with_no_parents_parses_like_a_struct() {
    let result = parse_text("class Account {\n  var balance: Int = 100\n  let rate: Int = 2\n}");
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
    let declaration = only_class(&result);
    assert!(declaration.parents.is_empty());
    assert_eq!(declaration.fields.len(), 2);
    assert!(declaration.overrides.is_empty());
}

#[test]
fn extends_takes_a_comma_separated_list() {
    let result = parse_text("class Combo extends Alpha, Beta, Gamma {}");
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
    let names: Vec<String> = only_class(&result)
        .parents
        .iter()
        .map(|parent| result.interner.resolve(parent.name).to_owned())
        .collect();
    assert_eq!(names, ["Alpha", "Beta", "Gamma"]);
}

#[test]
fn a_trailing_comma_in_the_parent_list_is_tolerated() {
    let result = parse_text("class Combo extends Alpha, Beta, {}");
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
    assert_eq!(only_class(&result).parents.len(), 2);
}

#[test]
fn override_let_parses_as_a_default_with_no_type() {
    let result = parse_text("class Savings extends Account {\n  override let rate = 5\n}");
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
    let declaration = only_class(&result);
    assert!(declaration.fields.is_empty());
    assert_eq!(declaration.overrides.len(), 1);
    assert_eq!(
        result.interner.resolve(declaration.overrides[0].name),
        "rate"
    );
}

#[test]
fn override_function_is_marked_and_plain_methods_are_not() {
    let result = parse_text(
        "class Savings extends Account {\n\
           function bonus() -> Int { return 1 }\n\
           override function tier() -> Int { return 2 }\n\
         }",
    );
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
    let flags: Vec<bool> = only_class(&result)
        .methods
        .iter()
        .map(|method| method.is_override)
        .collect();
    assert_eq!(flags, [false, true]);
}

#[test]
fn a_method_may_write_its_return_type_with_a_colon() {
    // Both `-> Int` and `): Int` are return-type spellings, and a class body is
    // where the second one shows up most.
    let result = parse_text("class Child extends Base {\n  function read(): Int { return 1 }\n}");
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
    assert_eq!(only_class(&result).methods.len(), 1);
}

#[test]
fn an_override_field_may_not_restate_a_type() {
    // The inherited field already decided its type; a second one could only
    // disagree with it.
    let result = parse_text("class Savings extends Account {\n  override let rate: Int = 5\n}");
    let codes: Vec<&str> = result
        .diagnostics
        .iter()
        .filter_map(|diagnostic| diagnostic.code)
        .collect();
    assert_eq!(codes, ["KPAR014"]);
}

#[test]
fn override_var_is_refused() {
    let result = parse_text("class Savings extends Account {\n  override var rate = 5\n}");
    let codes: Vec<&str> = result
        .diagnostics
        .iter()
        .filter_map(|diagnostic| diagnostic.code)
        .collect();
    assert_eq!(codes, ["KPAR013"]);
}

#[test]
fn a_malformed_member_is_reported_and_the_class_still_parses() {
    // Recovery is the point: one bad member must not swallow the rest of the
    // body or the items after the class.
    let result = parse_text(
        "class Account {\n  42\n  let rate: Int = 2\n}\n@Main function main() { return }",
    );
    let codes: Vec<&str> = result
        .diagnostics
        .iter()
        .filter_map(|diagnostic| diagnostic.code)
        .collect();
    assert_eq!(codes, ["KPAR009"]);
    match result.tree.items() {
        [Item::Class(declaration), Item::Function(_)] => {
            assert_eq!(declaration.fields.len(), 1);
        }
        items => panic!("expected a class then a function, got {items:?}"),
    }
}

#[test]
fn a_missing_parent_name_is_reported() {
    let result = parse_text("class Child extends {}");
    let codes: Vec<&str> = result
        .diagnostics
        .iter()
        .filter_map(|diagnostic| diagnostic.code)
        .collect();
    assert_eq!(codes, ["KPAR012"]);
}
