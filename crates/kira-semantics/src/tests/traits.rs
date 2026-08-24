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

/// `Eq` and an `Ord` that requires it, with a default calling across.
const ORDERED: &str = "trait Eq {\n    function equals(borrow self, other: Int) -> Bool\n}\n\
                       trait Ord: Eq {\n    function less(borrow self, other: Int) -> Bool\n\
                       \n    function atMost(borrow self, other: Int) -> Bool \
                       { return self.less(other) || self.equals(other) }\n}\n";

#[test]
fn a_type_claiming_both_a_trait_and_its_supertrait_is_accepted() {
    let items = diagnostics(&program(&format!(
        "{ORDERED}struct Mark: Eq, Ord {{\n    let n: Int\n\
         \n    function equals(borrow self, other: Int) -> Bool {{ return n == other }}\n\
         \n    function less(borrow self, other: Int) -> Bool {{ return n < other }}\n}}\n\
         function use(value: borrow Mark) -> Bool {{ return value.atMost(3) }}\n"
    )));
    assert!(items.is_empty(), "{items:?}");
}

#[test]
fn a_conformance_missing_the_supertrait_names_both() {
    let items = diagnostics(&program(&format!(
        "{ORDERED}struct Mark: Ord {{\n    let n: Int\n\
         \n    function less(borrow self, other: Int) -> Bool {{ return n < other }}\n}}\n"
    )));
    let refusal = items
        .iter()
        .find(|item| item.has_code("KSEM310"))
        .unwrap_or_else(|| panic!("expected a KSEM310, got {items:?}"));
    assert!(refusal.message.contains("`Ord`"), "{refusal:?}");
    assert!(refusal.message.contains("`Eq`"), "{refusal:?}");
}

#[test]
fn a_supertrait_may_be_kept_by_a_retroactive_block() {
    let items = diagnostics(&program(&format!(
        "{ORDERED}struct Mark: Ord {{\n    let n: Int\n\
         \n    function less(borrow self, other: Int) -> Bool {{ return n < other }}\n}}\n\
         extend Mark: Eq {{\n\
         \n    function equals(borrow self, other: Int) -> Bool {{ return n == other }}\n}}\n"
    )));
    assert!(items.is_empty(), "{items:?}");
}

#[test]
fn a_compiler_known_trait_may_be_required_as_a_supertrait() {
    let items = diagnostics(&program(
        "trait Cheap: Copyable {}\nstruct Label: Cheap {\n    let text: String\n}\n",
    ));
    let refusal = items
        .iter()
        .find(|item| item.has_code("KSEM310"))
        .unwrap_or_else(|| panic!("expected a KSEM310, got {items:?}"));
    assert!(refusal.message.contains("`Copyable`"), "{refusal:?}");
}

#[test]
fn a_supertrait_that_is_not_a_trait_is_refused() {
    let items = diagnostics(&program(
        "struct Point {\n    let x: Int\n}\ntrait Ordered: Point {}\n",
    ));
    let refusal = items
        .iter()
        .find(|item| item.has_code("KSEM308"))
        .unwrap_or_else(|| panic!("expected a KSEM308, got {items:?}"));
    assert!(refusal.message.contains("`Point`"), "{refusal:?}");
}

#[test]
fn a_supertrait_cycle_is_refused() {
    let items = diagnostics(&program("trait A: B {}\ntrait B: C {}\ntrait C: A {}\n"));
    let refusal = items
        .iter()
        .find(|item| item.has_code("KSEM309"))
        .unwrap_or_else(|| panic!("expected a KSEM309, got {items:?}"));
    assert!(refusal.message.contains("A -> B -> C -> A"), "{refusal:?}");
}

#[test]
fn a_trait_requiring_itself_is_refused() {
    let items = diagnostics(&program("trait Loop: Loop {}\n"));
    let codes: Vec<String> = items
        .iter()
        .filter_map(Diagnostic::code_text)
        .map(str::to_owned)
        .collect();
    assert_eq!(codes, vec!["KSEM309".to_owned()]);
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

/// A family whose declarations each answer for the trait it claims.
const SHAPES: &str = "trait Sized {\n    function area(borrow self) -> Int\n\
                      \n    function doubled(borrow self) -> Int { return self.area() * 2 }\n}\n\
                      construct Shape: Sized {\n    @Required function sides() -> Int\n}\n";

#[test]
fn a_construct_family_may_claim_a_trait_its_declarations_keep() {
    let items = diagnostics(&program(&format!(
        "{SHAPES}construct Square(edge: Int) extends Shape {{\n\
         \n    function sides() -> Int {{ return 4 }}\n\
         \n    function area(borrow self) -> Int {{ return edge * edge }}\n}}\n\
         function use(value: borrow Square) -> Int {{ return value.doubled() }}\n"
    )));
    assert!(items.is_empty(), "{items:?}");
}

#[test]
fn a_declaration_that_does_not_keep_its_familys_claim_names_both() {
    let items = diagnostics(&program(&format!(
        "{SHAPES}construct Bar(width: Int) extends Shape {{\n\
         \n    function sides() -> Int {{ return 4 }}\n}}\n"
    )));
    let refusal = items
        .iter()
        .find(|item| item.has_code("KSEM292"))
        .unwrap_or_else(|| panic!("expected a KSEM292, got {items:?}"));
    assert!(refusal.message.contains("`Bar`"), "{refusal:?}");
    assert!(refusal.message.contains("`Shape`"), "{refusal:?}");
}

#[test]
fn a_family_that_answers_the_trait_itself_discharges_it_for_every_declaration() {
    let items = diagnostics(&program(
        "trait Named {\n    function label(borrow self) -> String\n\
         \n    function shout(borrow self) -> String { return self.label() + \"!\" }\n}\n\
         construct Widget {\n    @Required function sides() -> Int\n}\n\
         extend Widget: Named {\n\
         \n    function label(borrow self) -> String { return \"widget\" }\n}\n\
         construct Panel(size: Int) extends Widget {\n\
         \n    function sides() -> Int { return 4 }\n}\n",
    ));
    assert!(items.is_empty(), "{items:?}");
}

#[test]
fn a_family_may_not_claim_a_compiler_known_trait() {
    let items = diagnostics(&program("construct Widget: Copyable {}\n"));
    let refusal = items
        .iter()
        .find(|item| item.has_code("KSEM298"))
        .unwrap_or_else(|| panic!("expected a KSEM298, got {items:?}"));
    assert!(refusal.message.contains("`Copyable`"), "{refusal:?}");
}

#[test]
fn a_family_claiming_something_that_is_not_a_trait_is_refused() {
    let items = diagnostics(&program(
        "struct Point {\n    let x: Int\n}\nconstruct Widget: Point {}\n",
    ));
    let codes: Vec<String> = items
        .iter()
        .filter_map(Diagnostic::code_text)
        .map(str::to_owned)
        .collect();
    assert!(codes.contains(&"KSEM289".to_owned()), "{items:?}");
}

/// A declaration that writes the claim itself keeps its own conformance: the
/// family's claim states what must be true of it, and it is.
#[test]
fn a_declaration_may_also_claim_the_trait_its_family_claims() {
    let items = diagnostics(&program(&format!(
        "{SHAPES}construct Square(edge: Int): Sized extends Shape {{\n\
         \n    function sides() -> Int {{ return 4 }}\n\
         \n    function area(borrow self) -> Int {{ return edge * edge }}\n}}\n"
    )));
    assert!(items.is_empty(), "{items:?}");
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
