//! Inferred construct fields, bare braced construction, and copy/update paths.

use crate::tests::{codes, library_codes};

#[test]
fn inferred_fields_bare_construction_and_nested_updates_check_cleanly() {
    assert!(
        codes(
            r#"
enum Material { Low XHigh }

struct Glass {
    var material: Material = .Low
}

construct Style {
    let additionalEffect: Int = 0
    let liquidGlass: Glass = Glass {}
    let score: Int { additionalEffect + (liquidGlass.material == .XHigh ? 10 : 0) }
}

Style Base {
    let additionalEffect = 3
    let liquidGlass = Glass {}
}

@Main
function main() {
    let StyleImplementation = Base {}
    let button = StyleImplementation { let additionalEffect = 8 }
    let sidebar = StyleImplementation {
        let liquidGlass.material = .XHigh
    }
    print(button.score)
    print(sidebar.score)
    return
}
"#,
        )
        .is_empty()
    );
}

#[test]
fn later_construct_members_may_use_earlier_members() {
    assert!(
        codes(
            r#"
construct Theme {
    let value: Int = 0
}

Theme Concrete {
    let value = 3
    let doubled = value + 1
}

@Main
function main() {
    print(Concrete {}.doubled)
    return
}
"#,
        )
        .is_empty()
    );
}

#[test]
fn forward_construct_member_references_are_rejected() {
    let diagnostics = library_codes(
        r#"
construct Theme {}

Theme Broken {
    let later = earlier
    let earlier = 1
}
"#,
    );
    assert!(
        diagnostics.iter().any(|code| code == "KSEM060"),
        "{diagnostics:?}"
    );
}

#[test]
fn inferred_construct_value_cycles_are_rejected_before_lowering() {
    let diagnostics = library_codes(
        r#"
construct Theme {}

Theme Loop {
    let next = Loop {}
}
"#,
    );
    assert!(
        diagnostics.iter().any(|code| code == "KSEM052"),
        "{diagnostics:?}"
    );
}

#[test]
fn a_bare_braced_construct_still_fills_child_content() {
    assert!(
        codes(
            r#"
construct Child {
    @Required let value: Int
}

Child Leaf(value: Int) {
    let result: Int { value }
}

construct Stack {
    let child: some Child
    let result: Int { child.value }
}

Stack Wrap {
    let child: some Child
}

@Main
function main() {
    let stack = Wrap { Leaf(value: 7) }
    print(stack.result)
    return
}
"#,
        )
        .is_empty()
    );
}

#[test]
fn an_unannotated_construct_member_needs_a_type_or_initializer() {
    assert_eq!(
        library_codes(
            r#"
construct Style {
    let value: Int { 0 }
}

Style Broken {
    let missing
}
"#,
        ),
        vec!["KSEM261"]
    );
}

#[test]
fn construct_update_paths_report_unknown_intermediate_and_overlap_errors() {
    let diagnostics = library_codes(
        r#"
struct Glass {
    var material: Int = 0
}

construct Style {
    let value: Int = 0
    let glass: Glass = Glass {}
}

Style Base {
    let value: Int = 0
    let glass: Glass = Glass {}
}

function check() {
    let base = Base {}
    let unknown = base { let missing.path = 1 }
    let invalid = base { let value.child = 1 }
    let duplicate = base {
        let value = 1
        let value = 2
    }
    return
}
"#,
    );
    assert!(diagnostics.iter().any(|code| code == "KSEM267"));
    assert!(
        diagnostics.iter().any(|code| code == "KSEM268"),
        "{diagnostics:?}"
    );
    assert!(diagnostics.iter().any(|code| code == "KSEM265"));
}

#[test]
fn an_uninferrable_empty_array_stays_a_type_inference_diagnostic() {
    let diagnostics = library_codes(
        r#"
construct Style {
    let value: Int { 0 }
}

Style Broken {
    let values = []
}
"#,
    );
    assert!(
        diagnostics.iter().any(|code| code == "KSEM104"),
        "{diagnostics:?}"
    );
}

/// A defaulted stored member with a written type is a value member of the
/// family, readable through `Any Family` anywhere — not only where
/// specialization happens to substitute the concrete declaration.
#[test]
fn a_typed_stored_member_reads_through_the_family_value() {
    assert!(
        codes(
            r#"
construct Style {
    @Required let colors: Int
    let appearance: Int = 2
}

Style Base {
    let colors = 7
}

function readThrough(style: Any Style) -> Int {
    return style.appearance + style.colors
}

@Main
function main() {
    print(readThrough(Base {}))
    return
}
"#,
        )
        .is_empty()
    );
}

/// A stored member with no written type has no result type a family-value read
/// could carry, and the refusal names the member and the fix.
#[test]
fn an_untyped_stored_member_read_through_the_family_is_refused() {
    assert_eq!(
        library_codes(
            r#"
construct Style {
    @Required let colors: Int
    let appearance = 2
}

Style Base {
    let colors = 7
}

function readThrough(style: Any Style) -> Int {
    return style.appearance
}
"#,
        ),
        vec!["KSEM271"]
    );
}
