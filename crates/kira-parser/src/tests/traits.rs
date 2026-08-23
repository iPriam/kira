//! Trait declarations, the `: Trait` conformance clause, and the `self`
//! receiver.

use super::*;
use kira_syntax_model::ast::TraitDecl;

/// The one trait in `text`, for tests that parse a single declaration.
fn only_trait(result: &ParseResult) -> &TraitDecl {
    match result.tree.items() {
        [Item::Trait(declaration)] => declaration,
        items => panic!("expected exactly one trait, got {items:?}"),
    }
}

#[test]
fn a_bodyless_member_is_a_requirement_and_a_bodied_one_is_a_default() {
    let result = parse_text(
        "trait Hashable {\n    function hash(borrow self) -> Int\n\
         \n    function label(borrow self) -> String { return \"h\" }\n}\n",
    );
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
    let declaration = only_trait(&result);
    let shapes: Vec<(String, bool)> = declaration
        .members
        .iter()
        .map(|member| {
            (
                result.interner.resolve(member.function.name).to_owned(),
                member.has_body,
            )
        })
        .collect();
    assert_eq!(
        shapes,
        vec![("hash".to_owned(), false), ("label".to_owned(), true)]
    );
}

#[test]
fn a_marker_trait_declares_no_members() {
    let result = parse_text("trait Send {}\n");
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
    assert!(only_trait(&result).members.is_empty());
}

#[test]
fn a_trailing_semicolon_ends_a_requirement() {
    let result = parse_text("trait Hashable {\n    function hash(borrow self) -> Int;\n}\n");
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
    assert_eq!(only_trait(&result).members.len(), 1);
}

#[test]
fn a_supertrait_clause_is_recorded_rather_than_dropped() {
    let result = parse_text("trait Ordered: Hashable {}\n");
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
    let names: Vec<String> = only_trait(&result)
        .supertraits
        .iter()
        .map(|entry| result.interner.resolve(entry.name).to_owned())
        .collect();
    assert_eq!(names, vec!["Hashable".to_owned()]);
}

#[test]
fn a_struct_records_its_conformance_list() {
    let result = parse_text("struct Mesh: Hashable, Send {\n    let id: Int\n}\n");
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
    let declaration = only_struct(&result);
    let names: Vec<String> = declaration
        .traits
        .iter()
        .map(|entry| result.interner.resolve(entry.name).to_owned())
        .collect();
    assert_eq!(names, vec!["Hashable".to_owned(), "Send".to_owned()]);
}

#[test]
fn a_class_takes_traits_then_parents() {
    let result = parse_text("class Panel: Drawable extends Surface {\n    let id: Int\n}\n");
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
    let [Item::Class(declaration)] = result.tree.items() else {
        panic!("expected a class, got {:?}", result.tree.items());
    };
    let traits: Vec<String> = declaration
        .traits
        .iter()
        .map(|entry| result.interner.resolve(entry.name).to_owned())
        .collect();
    let parents: Vec<String> = declaration
        .parents
        .iter()
        .map(|parent| result.interner.resolve(parent.name).to_owned())
        .collect();
    assert_eq!(traits, vec!["Drawable".to_owned()]);
    assert_eq!(parents, vec!["Surface".to_owned()]);
}

#[test]
fn a_construct_family_takes_traits_then_parents() {
    let result = parse_text("construct Child: Hashable extends Parent {}\n");
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
    let [Item::Construct(declaration)] = result.tree.items() else {
        panic!("expected a construct, got {:?}", result.tree.items());
    };
    assert_eq!(declaration.traits.len(), 1);
    assert_eq!(declaration.extends.len(), 1);
}

#[test]
fn an_extend_block_with_a_colon_is_an_impl_block() {
    let result = parse_text(
        "extend Mesh: Hashable {\n    function hash(borrow self) -> Int { return 1 }\n}\n",
    );
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
    let [Item::Extend(declaration)] = result.tree.items() else {
        panic!("expected an extend, got {:?}", result.tree.items());
    };
    let claimed = declaration.conforms.expect("the block names a trait");
    assert_eq!(result.interner.resolve(claimed.name), "Hashable");
}

#[test]
fn an_extend_block_without_a_colon_stays_a_modifier_block() {
    let result =
        parse_text("extend Widget {\n    function padding(amount: Int) -> Int { return 1 }\n}\n");
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
    let [Item::Extend(declaration)] = result.tree.items() else {
        panic!("expected an extend, got {:?}", result.tree.items());
    };
    assert!(declaration.conforms.is_none());
}

#[test]
fn an_impl_block_naming_two_traits_is_refused() {
    let result = parse_text("extend Mesh: Hashable, Send {}\n");
    let codes: Vec<&str> = result
        .diagnostics
        .iter()
        .filter_map(|item| item.code_text())
        .collect();
    assert_eq!(codes, vec!["KPAR076"]);
}

#[test]
fn a_borrow_mut_receiver_is_recorded_and_takes_no_parameter_slot() {
    let result = parse_text(
        "struct Counter {\n    var ticks: Int\n\
         \n    function bump(borrow mut self, by: Int) { return }\n}\n",
    );
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
    let method = &only_struct(&result).methods[0];
    let receiver = method.receiver.expect("a written receiver");
    assert!(receiver.mutable);
    assert_eq!(method.params.len(), 1);
}

#[test]
fn a_borrow_receiver_is_the_immutable_one() {
    let result =
        parse_text("struct Mesh {\n    function hash(borrow self) -> Int { return 1 }\n}\n");
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
    let receiver = only_struct(&result).methods[0]
        .receiver
        .expect("a written receiver");
    assert!(!receiver.mutable);
}

#[test]
fn a_bare_self_receiver_is_refused_by_name() {
    let result = parse_text("struct Mesh {\n    function hash(self) -> Int { return 1 }\n}\n");
    let codes: Vec<&str> = result
        .diagnostics
        .iter()
        .filter_map(|item| item.code_text())
        .collect();
    assert_eq!(codes, vec!["KPAR075"]);
}

#[test]
fn a_parameter_named_self_still_parses_as_a_parameter() {
    let result = parse_text("function run(self: Int) -> Int { return self }\n");
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
    let function = only_function(&result);
    assert!(function.receiver.is_none());
    assert_eq!(function.params.len(), 1);
}

#[test]
fn a_trait_takes_no_type_parameters() {
    let result = parse_text("trait Holder<Value> {}\n");
    assert!(
        result
            .diagnostics
            .iter()
            .any(|item| item.message.contains("trait")),
        "{:?}",
        result.diagnostics
    );
}

#[test]
fn a_non_function_member_is_refused_and_the_rest_of_the_trait_still_parses() {
    let result = parse_text(
        "trait Hashable {\n    let seed: Int\n\
         \n    function hash(borrow self) -> Int\n}\n",
    );
    let codes: Vec<&str> = result
        .diagnostics
        .iter()
        .filter_map(|item| item.code_text())
        .collect();
    assert_eq!(codes, vec!["KPAR073"]);
    assert_eq!(only_trait(&result).members.len(), 1);
}
