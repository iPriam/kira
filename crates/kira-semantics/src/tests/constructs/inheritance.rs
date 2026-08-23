//! `construct Child extends Parent`: the surface a child takes on, the variants
//! a parent gains, and what a child may and may not change.

use super::super::library_codes;

/// A declaration backed by a child family is a value of the parent's type.
///
/// This is the whole point of the clause: a runtime that holds `[Any Parent]`
/// drives declarations written against families it has never heard of.
#[test]
fn a_child_families_declarations_are_values_of_the_parent() {
    let source = r#"
construct Parent {
    @Required function label() -> String
}

construct Child extends Parent {
    @Required function label() -> String
}

construct One() extends Child { label { return "one" } }

function collect() -> Int {
    let all: [Any Parent] = [One()]
    return all.count
}
"#;
    assert!(
        library_codes(source).is_empty(),
        "{:?}",
        library_codes(source)
    );
}

/// A child inherits what the parent required without restating it, and a
/// declaration that leaves it unimplemented is refused against the child.
#[test]
fn a_child_inherits_its_parents_requirements() {
    let source = r#"
construct Parent {
    @Required function label() -> String
}

construct Child extends Parent {}

construct One() extends Child {}
"#;
    assert!(
        library_codes(source).iter().any(|code| code == "KSEM234"),
        "{:?}",
        library_codes(source)
    );
}

/// A child may make a result more specific.
#[test]
fn a_child_may_narrow_a_result() {
    let source = r#"
construct Parent {
    @Required function render() -> Any
}

construct Child extends Parent {
    @Required function render() -> String
}

construct One() extends Child { render { return "one" } }
"#;
    assert!(
        library_codes(source).is_empty(),
        "{:?}",
        library_codes(source)
    );
}

/// A child may not make a parameter more specific.
///
/// Narrowing a result is safe because a caller through `Any Parent` receives
/// something more specific than it was promised. Narrowing a *parameter* is the
/// opposite: the caller hands over whatever the parent's signature accepts, and
/// the child would refuse a value the parent said it would take.
#[test]
fn a_child_may_not_narrow_a_parameter() {
    let source = r#"
construct Parent {
    @Required function accept(value: Any) -> Bool
}

construct Child extends Parent {
    @Required function accept(value: String) -> Bool
}
"#;
    assert!(
        library_codes(source).iter().any(|code| code == "KSEM206"),
        "{:?}",
        library_codes(source)
    );
}

/// A child may not answer with something unrelated to what the parent promised.
#[test]
fn a_child_may_not_change_a_result_to_an_unrelated_type() {
    let source = r#"
construct Parent {
    @Required function size() -> Int
}

construct Child extends Parent {
    @Required function size() -> String
}
"#;
    assert!(
        library_codes(source).iter().any(|code| code == "KSEM206"),
        "{:?}",
        library_codes(source)
    );
}

/// A parent named by a family that is not one is refused where it is written.
#[test]
fn extending_something_that_is_not_a_family_is_refused() {
    let source = r#"
struct Plain { let value: Int = 0 }

construct Child extends Plain {}
"#;
    assert!(
        library_codes(source).iter().any(|code| code == "KSEM200"),
        "{:?}",
        library_codes(source)
    );
}

/// A cycle is refused rather than broken at an arbitrary edge.
#[test]
fn an_inheritance_cycle_is_refused() {
    let source = r#"
construct Left extends Right {}
construct Right extends Left {}
"#;
    assert!(
        library_codes(source).iter().any(|code| code == "KSEM205"),
        "{:?}",
        library_codes(source)
    );
}

/// A family cannot extend itself.
#[test]
fn a_family_cannot_extend_itself() {
    let source = "construct Loop extends Loop {}\n";
    assert!(
        library_codes(source).iter().any(|code| code == "KSEM204"),
        "{:?}",
        library_codes(source)
    );
}

/// Inheritance is transitive: a grandchild's declarations reach the top.
#[test]
fn a_grandchilds_declarations_reach_the_topmost_family() {
    let source = r#"
construct Top {
    @Required function label() -> String
}

construct Middle extends Top {}

construct Bottom extends Middle {}

construct One() extends Bottom { label { return "one" } }

function collect() -> Int {
    let all: [Any Top] = [One()]
    return all.count
}
"#;
    assert!(
        library_codes(source).is_empty(),
        "{:?}",
        library_codes(source)
    );
}
