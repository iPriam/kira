//! Trait conformance: what a claim obliges, what it inherits, and what it is
//! refused for.

use super::*;

const MAIN: &str = "@Main function main() { return }\n";

/// A program whose trait, conforming type, and `@Main` are the whole file.
fn program(body: &str) -> String {
    format!("{body}{MAIN}")
}

const SCORED: &str = "trait Scored {\n    function score(borrow self) -> Int\n\
                      \n    function doubled(borrow self) -> Int { return self.score() * 2 }\n}\n";

#[test]
fn a_conforming_type_that_implements_its_requirement_is_accepted() {
    let items = diagnostics(&program(&format!(
        "{SCORED}struct Leaf: Scored {{\n    let n: Int\n\
         \n    function score(borrow self) -> Int {{ return n }}\n}}\n"
    )));
    assert!(items.is_empty(), "{items:?}");
}

#[test]
fn an_unimplemented_requirement_names_the_member_and_the_shape() {
    let items = diagnostics(&program(&format!(
        "{SCORED}struct Leaf: Scored {{\n    let n: Int\n}}\n"
    )));
    let refusal = items
        .iter()
        .find(|item| item.has_code("KSEM292"))
        .unwrap_or_else(|| panic!("expected a KSEM292, got {items:?}"));
    assert!(
        refusal.message.contains("presents no `score`"),
        "{refusal:?}"
    );
    assert!(
        refusal.message.contains("function score() -> Int"),
        "{refusal:?}"
    );
}

#[test]
fn an_implementation_with_the_wrong_result_is_refused() {
    let items = diagnostics(&program(&format!(
        "{SCORED}struct Leaf: Scored {{\n    let n: Int\n\
         \n    function score(borrow self) -> String {{ return \"x\" }}\n}}\n"
    )));
    let codes: Vec<String> = items
        .iter()
        .filter_map(Diagnostic::code_text)
        .map(str::to_owned)
        .collect();
    assert!(codes.contains(&"KSEM293".to_owned()), "{items:?}");
}

#[test]
fn an_implementation_with_the_wrong_parameters_is_refused() {
    let items = diagnostics(&program(
        "trait Accepts {\n    function take(borrow self, value: Int) -> Bool\n}\n\
         struct Gate: Accepts {\n\
         \n    function take(borrow self, value: String) -> Bool { return true }\n}\n",
    ));
    let codes: Vec<String> = items
        .iter()
        .filter_map(Diagnostic::code_text)
        .map(str::to_owned)
        .collect();
    assert!(codes.contains(&"KSEM293".to_owned()), "{items:?}");
}

#[test]
fn a_default_is_inherited_and_reaches_the_conforming_types_own_body() {
    let program = program(&format!(
        "{SCORED}struct Leaf: Scored {{\n    let n: Int\n\
         \n    function score(borrow self) -> Int {{ return n }}\n}}\n\
         function use(value: borrow Leaf) -> Int {{ return value.doubled() }}\n"
    ));
    let items = diagnostics(&program);
    assert!(items.is_empty(), "{items:?}");
}

#[test]
fn a_type_that_writes_the_default_itself_does_not_inherit_a_second_copy() {
    let items = diagnostics(&program(&format!(
        "{SCORED}struct Leaf: Scored {{\n    let n: Int\n\
         \n    function score(borrow self) -> Int {{ return n }}\n\
         \n    function doubled(borrow self) -> Int {{ return n * 3 }}\n}}\n"
    )));
    assert!(items.is_empty(), "{items:?}");
}

#[test]
fn a_retroactive_impl_block_implements_the_trait() {
    let items = diagnostics(&program(&format!(
        "{SCORED}struct Leaf {{\n    let n: Int\n}}\n\
         extend Leaf: Scored {{\n    function score(borrow self) -> Int {{ return n }}\n}}\n\
         function use(value: borrow Leaf) -> Int {{ return value.doubled() }}\n"
    )));
    assert!(items.is_empty(), "{items:?}");
}

#[test]
fn conforming_twice_is_refused() {
    let items = diagnostics(&program(&format!(
        "{SCORED}struct Leaf: Scored {{\n    let n: Int\n\
         \n    function score(borrow self) -> Int {{ return n }}\n}}\n\
         extend Leaf: Scored {{\n    function score(borrow self) -> Int {{ return n }}\n}}\n"
    )));
    let codes: Vec<String> = items
        .iter()
        .filter_map(Diagnostic::code_text)
        .map(str::to_owned)
        .collect();
    assert!(codes.contains(&"KSEM290".to_owned()), "{items:?}");
}

#[test]
fn an_impl_block_may_not_add_a_member_the_trait_never_declared() {
    let items = diagnostics(&program(&format!(
        "{SCORED}struct Leaf {{\n    let n: Int\n}}\n\
         extend Leaf: Scored {{\n    function score(borrow self) -> Int {{ return n }}\n\
         \n    function extra(borrow self) -> Int {{ return 1 }}\n}}\n"
    )));
    let refusal = items
        .iter()
        .find(|item| item.has_code("KSEM294"))
        .unwrap_or_else(|| panic!("expected a KSEM294, got {items:?}"));
    assert!(refusal.message.contains("`extra`"), "{refusal:?}");
}

#[test]
fn a_name_that_is_not_a_trait_is_refused_at_the_conformance() {
    let items = diagnostics(&program("struct Leaf: Missing {\n    let n: Int\n}\n"));
    let codes: Vec<String> = items
        .iter()
        .filter_map(Diagnostic::code_text)
        .map(str::to_owned)
        .collect();
    assert_eq!(codes, vec!["KSEM289".to_owned()]);
}

#[test]
fn a_trait_may_not_take_a_name_another_declaration_holds() {
    let items = diagnostics(&program(
        "struct Mesh {\n    let n: Int\n}\ntrait Mesh {}\n",
    ));
    let refusal = items
        .iter()
        .find(|item| item.has_code("KSEM288"))
        .unwrap_or_else(|| panic!("expected a KSEM288, got {items:?}"));
    assert!(refusal.message.contains("a struct"), "{refusal:?}");
}

#[test]
fn a_compiler_known_trait_may_not_be_declared() {
    let items = diagnostics(&program("trait Copyable {}\n"));
    let codes: Vec<String> = items
        .iter()
        .filter_map(Diagnostic::code_text)
        .map(str::to_owned)
        .collect();
    assert_eq!(codes, vec!["KSEM288".to_owned()]);
}

#[test]
fn a_supertrait_is_refused_by_name() {
    let items = diagnostics(&program("trait Base {}\ntrait Ordered: Base {}\n"));
    let refusal = items
        .iter()
        .find(|item| item.has_code("KSEM296"))
        .unwrap_or_else(|| panic!("expected a KSEM296, got {items:?}"));
    assert!(refusal.message.contains("supertrait"), "{refusal:?}");
}

#[test]
fn a_trait_names_no_type() {
    let items = diagnostics(&program(
        "trait Scored {}\nfunction take(value: Scored) -> Int { return 1 }\n",
    ));
    let codes: Vec<String> = items
        .iter()
        .filter_map(Diagnostic::code_text)
        .map(str::to_owned)
        .collect();
    assert!(codes.contains(&"KSEM295".to_owned()), "{items:?}");
}

#[test]
fn a_construct_family_cannot_claim_a_trait() {
    let items = diagnostics(&program("trait Scored {}\nconstruct Widget: Scored {}\n"));
    let codes: Vec<String> = items
        .iter()
        .filter_map(Diagnostic::code_text)
        .map(str::to_owned)
        .collect();
    assert!(codes.contains(&"KSEM298".to_owned()), "{items:?}");
}

#[test]
fn an_impl_block_for_a_name_that_is_no_type_is_refused() {
    let items = diagnostics(&program("trait Scored {}\nextend Nothing: Scored {}\n"));
    let codes: Vec<String> = items
        .iter()
        .filter_map(Diagnostic::code_text)
        .map(str::to_owned)
        .collect();
    assert!(codes.contains(&"KSEM298".to_owned()), "{items:?}");
}

#[test]
fn an_eligible_copyable_claim_is_accepted() {
    let items = diagnostics(&program(
        "struct Point: Copyable {\n    let x: Int\n    let y: Int\n}\n",
    ));
    assert!(items.is_empty(), "{items:?}");
}

#[test]
fn an_ineligible_copyable_claim_names_the_offending_member() {
    let items = diagnostics(&program(
        "struct Label: Copyable {\n    let id: Int\n    let text: String\n}\n",
    ));
    let refusal = items
        .iter()
        .find(|item| item.has_code("KSEM297"))
        .unwrap_or_else(|| panic!("expected a KSEM297, got {items:?}"));
    assert!(refusal.message.contains("`text`"), "{refusal:?}");
    assert!(refusal.message.contains("`String`"), "{refusal:?}");
}

#[test]
fn a_receiver_on_a_free_function_is_refused() {
    let items = diagnostics(&program("function run(borrow self) -> Int { return 1 }\n"));
    let codes: Vec<String> = items
        .iter()
        .filter_map(Diagnostic::code_text)
        .map(str::to_owned)
        .collect();
    assert!(codes.contains(&"KSEM299".to_owned()), "{items:?}");
}

#[test]
fn a_class_and_its_subclass_both_keep_the_promise() {
    let items = diagnostics(&program(&format!(
        "{SCORED}class Base: Scored {{\n    let seed: Int = 4\n\
         \n    function score(borrow self) -> Int {{ return seed }}\n}}\n\
         class Derived: Scored extends Base {{\n\
         \n    override function score(borrow self) -> Int {{ return seed * 2 }}\n}}\n\
         function use(value: borrow Derived) -> Int {{ return value.doubled() }}\n"
    )));
    assert!(items.is_empty(), "{items:?}");
}

/// The diagnostics of a program built against dependency *packages* rather
/// than sibling modules.
///
/// Coherence is a rule about packages, so it cannot be exercised through the
/// flat module scope the other tests use: every file of one program shares one
/// package, and a conformance written there is always the type's own.
fn package_codes(text: &str, packages: &[(&str, &str)]) -> Vec<String> {
    let db = salsa::DatabaseImpl::new();
    let modules: Vec<ModuleSource> = packages
        .iter()
        .map(|&(package, text)| ModuleSource {
            module: ImportTable::package_module_identity(package, package),
            path: format!("{package}/{package}.kira"),
            text: text.to_owned(),
        })
        .collect();
    let source = SourceProgram::application(&db, text.to_owned(), "main.kira".to_owned(), modules);
    analyzed::accumulated::<DiagnosticAccumulator>(&db, source)
        .into_iter()
        .filter_map(|accumulator| accumulator.0.code_text().map(str::to_owned))
        .collect()
}

#[test]
fn a_conformance_declared_by_a_third_package_is_refused() {
    let codes = package_codes(
        "import Shapes\nimport Marks\n\
         extend Mesh: Scored { function score(borrow self) -> Int { return 1 } }\n\
         @Main function main() { return }\n",
        &[
            ("Shapes", "struct Mesh {\n    let n: Int\n}\n"),
            (
                "Marks",
                "trait Scored {\n    function score(borrow self) -> Int\n}\n",
            ),
        ],
    );
    assert!(codes.contains(&"KSEM291".to_owned()), "{codes:?}");
}

#[test]
fn a_conformance_declared_by_the_traits_own_package_is_accepted() {
    let codes = package_codes(
        "import Shapes\nimport Marks\n@Main function main() { return }\n",
        &[
            ("Shapes", "struct Mesh {\n    let n: Int\n}\n"),
            (
                "Marks",
                "import Shapes\ntrait Scored {\n    function score(borrow self) -> Int\n}\n\
                 extend Mesh: Scored {\n    function score(borrow self) -> Int { return n }\n}\n",
            ),
        ],
    );
    assert!(codes.is_empty(), "{codes:?}");
}

const DROPPING: &str = "struct Handle: Drop {\n    let id: Int\n\
                        \n    function drop(borrow mut self) { return }\n}\n";

#[test]
fn a_drop_conformance_with_a_drop_member_is_accepted() {
    let items = diagnostics(&program(DROPPING));
    assert!(items.is_empty(), "{items:?}");
}

#[test]
fn a_drop_conformance_with_no_drop_member_is_refused() {
    let items = diagnostics(&program("struct Handle: Drop {\n    let id: Int\n}\n"));
    let refusal = items
        .iter()
        .find(|item| item.has_code("KSEM301"))
        .unwrap_or_else(|| panic!("expected a KSEM301, got {items:?}"));
    assert!(
        refusal.message.contains("presents no `drop`"),
        "{refusal:?}"
    );
}

#[test]
fn a_drop_member_that_takes_or_returns_anything_is_refused() {
    let items = diagnostics(&program(
        "struct Handle: Drop {\n    let id: Int\n\
         \n    function drop(borrow mut self, extra: Int) { return }\n}\n",
    ));
    let codes: Vec<String> = items
        .iter()
        .filter_map(Diagnostic::code_text)
        .map(str::to_owned)
        .collect();
    assert!(codes.contains(&"KSEM301".to_owned()), "{items:?}");
}

#[test]
fn calling_drop_by_name_is_refused() {
    let items = diagnostics(&format!(
        "{DROPPING}@Main function main() {{ let h = Handle(id: 1) h.drop() return }}\n"
    ));
    let refusal = items
        .iter()
        .find(|item| item.has_code("KSEM300"))
        .unwrap_or_else(|| panic!("expected a KSEM300, got {items:?}"));
    assert!(
        refusal.message.contains("run by the release"),
        "{refusal:?}"
    );
}

#[test]
fn a_drop_type_is_not_copyable() {
    let items = diagnostics(&program(
        "struct Handle: Copyable, Drop {\n    let id: Int\n\
         \n    function drop(borrow mut self) { return }\n}\n",
    ));
    let refusal = items
        .iter()
        .find(|item| item.has_code("KSEM297"))
        .unwrap_or_else(|| panic!("expected a KSEM297, got {items:?}"));
    assert!(refusal.message.contains("`Drop`"), "{refusal:?}");
}

#[test]
fn a_drop_type_moves_when_it_is_bound() {
    let items = diagnostics(&format!(
        "{DROPPING}@Main function main() {{ let a = Handle(id: 1) let b = a print(a.id) return }}\n"
    ));
    assert!(!items.is_empty(), "binding a `Drop` value moves it");
}

#[test]
fn a_drop_body_is_registered_as_the_types_glue() {
    let program = analyze_text(&program(DROPPING));
    let id = program
        .types
        .structs()
        .lookup("Handle")
        .expect("the struct is declared");
    let def = program.types.structs().get(id).expect("the id resolves");
    assert!(def.drop_glue.is_some(), "the body is recorded on the type");
    // A type that runs a body is released wherever it is held, even when every
    // member it holds is a scalar.
    assert!(program.types.owns_heap(Type::Struct(id)));
    assert!(program.types.moves_on_bind(Type::Struct(id)));
}

#[test]
fn a_type_holding_a_drop_value_runs_one_too() {
    let program = analyze_text(&program(&format!(
        "{DROPPING}struct Box {{\n    let held: Handle\n}}\n"
    )));
    let id = program
        .types
        .structs()
        .lookup("Box")
        .expect("the struct is declared");
    assert!(program.types.runs_user_drop(Type::Struct(id)));
    assert!(program.types.moves_on_bind(Type::Struct(id)));
}

#[test]
fn an_impl_block_for_drop_may_declare_only_drop() {
    let items = diagnostics(&program(
        "struct Handle {\n    let id: Int\n}\n\
         extend Handle: Drop {\n    function drop(borrow mut self) { return }\n\
         \n    function extra(borrow self) -> Int { return 1 }\n}\n",
    ));
    let codes: Vec<String> = items
        .iter()
        .filter_map(Diagnostic::code_text)
        .map(str::to_owned)
        .collect();
    assert!(codes.contains(&"KSEM294".to_owned()), "{items:?}");
}
