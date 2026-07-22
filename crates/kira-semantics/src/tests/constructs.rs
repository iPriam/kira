//! Semantic analysis of the construct declaration family: a family template, a
//! construct-backed declaration that conforms to one, its construction, and the
//! read of its computed bridge member — plus every typed refusal.

use super::{codes, library_codes};

/// A family, a conforming backed declaration, its construction, and a bridge
/// read all type-check cleanly.
#[test]
fn a_construct_backed_declaration_and_its_bridge_read_check_clean() {
    assert!(
        codes(
            r#"
construct Shape {
    @Required let sides: Int
    let area: Int { 0 }
}

Shape Square(side: Int) {
    let area: Int { side * side }
}

@Main
function main() {
    let s = Square(side: 5)
    print(s.area)
    return
}
"#,
        )
        .is_empty()
    );
}

/// A construction input may be passed positionally too.
#[test]
fn a_construction_input_binds_positionally() {
    assert!(
        codes(
            r#"
construct Shape {
    let area: Int { 0 }
}

Shape Square(side: Int) {
    let area: Int { side * side }
}

@Main
function main() {
    print(Square(5).area)
    return
}
"#,
        )
        .is_empty()
    );
}

#[test]
fn a_backed_declaration_of_an_unknown_family_is_refused() {
    assert_eq!(
        library_codes(
            r#"
Widget Text(content: Int) {
    let node: Int { content }
}
"#,
        ),
        vec!["KSEM200"]
    );
}

/// A required family member the declaration neither provides nor discharges by
/// overriding the family's bridge is refused.
#[test]
fn a_missing_required_member_is_refused() {
    assert!(
        library_codes(
            r#"
construct Widget {
    @Required let body: Int
    let node: Int { body }
}

Widget Composite(tag: Int) {
    function unrelated() -> Int {
        return tag
    }
}
"#,
        )
        // The inherited default bridge then reads the absent `body`, so an
        // undefined-name diagnostic follows the missing-member refusal.
        .contains(&"KSEM201")
    );
}

/// Overriding the family's bridge discharges the required member: a terminal
/// declaration need not provide `body`.
#[test]
fn overriding_the_bridge_discharges_the_required_member() {
    assert!(
        library_codes(
            r#"
construct Widget {
    @Required let body: Int
    let node: Int { body }
}

Widget Leaf(text: Int) {
    let node: Int { text }
}
"#,
        )
        .is_empty()
    );
}

#[test]
fn a_duplicate_member_is_refused() {
    assert_eq!(
        library_codes(
            r#"
construct Family {
    let node: Int { 0 }
}

Family Thing(value: Int) {
    let value: Int = 1
    let node: Int { value }
}
"#,
        ),
        vec!["KSEM202"]
    );
}

#[test]
fn a_content_slot_over_a_family_type_is_an_executable_heterogeneous_field() {
    assert!(
        library_codes(
            r#"
construct Family {
    function value() -> Int { return 0 }
}

Family Leaf(number: Int) {
    function value() -> Int { return number }
}

Family Stack() {
    @Content let children: [Family]
    function value() -> Int { return children.count }
}
"#,
        )
        .is_empty()
    );
}

/// A child slot over a concrete type is a real field and checks clean.
#[test]
fn a_concrete_child_slot_checks_clean() {
    assert!(
        library_codes(
            r#"
struct Leaf {
    var value: Int = 0
}

construct Family {
    let node: Int { 0 }
}

Family One() {
    let child: some Leaf
    let node: Int { child.value }
}

Family Many() {
    let items: [some Leaf]
    let node: Int { items.count }
}
"#,
        )
        .is_empty()
    );
}

/// A construction fills a single slot and a list slot from its trailing
/// children, and the whole program checks clean.
#[test]
fn a_construction_fills_its_child_slots() {
    assert!(
        codes(
            r#"
struct Leaf {
    var value: Int = 0
}

construct Family {
    let count: Int { 0 }
}

Family One() {
    let child: some Leaf
    let count: Int { 1 }
}

Family Many() {
    let items: [some Leaf]
    let count: Int { items.count }
}

@Main
function main() {
    let a = One() { Leaf { value = 3 } }
    print(a.count)
    let b = Many() { Leaf { value = 1 } Leaf { value = 2 } }
    print(b.count)
    return
}
"#,
        )
        .is_empty()
    );
}

/// A child whose type does not satisfy the slot's element type is refused.
#[test]
fn a_wrong_typed_child_is_refused() {
    assert_eq!(
        codes(
            r#"
struct Leaf {
    var value: Int = 0
}

construct Family {
    let count: Int { 0 }
}

Family One() {
    let child: some Leaf
    let count: Int { 1 }
}

@Main
function main() {
    let a = One() { 42 }
    print(a.count)
    return
}
"#,
        ),
        vec!["KSEM232"]
    );
}

/// A single slot takes exactly one child: two is a count mismatch.
#[test]
fn too_many_children_for_a_single_slot_is_refused() {
    assert_eq!(
        codes(
            r#"
struct Leaf {
    var value: Int = 0
}

construct Family {
    let count: Int { 0 }
}

Family One() {
    let child: some Leaf
    let count: Int { 1 }
}

@Main
function main() {
    let a = One() { Leaf {} Leaf {} }
    print(a.count)
    return
}
"#,
        ),
        vec!["KSEM231"]
    );
}

/// Children on a construction whose declaration has no child slot are refused.
#[test]
fn children_on_a_slotless_construct_are_refused() {
    assert_eq!(
        codes(
            r#"
struct Leaf {
    var value: Int = 0
}

construct Family {
    let count: Int { 0 }
}

Family Plain(tag: Int) {
    let count: Int { tag }
}

@Main
function main() {
    let a = Plain(tag: 1) { Leaf {} }
    print(a.count)
    return
}
"#,
        ),
        vec!["KSEM229"]
    );
}

/// A trailing content block on something that is not a construct-backed
/// declaration is refused.
#[test]
fn children_on_a_non_construct_are_refused() {
    assert_eq!(
        codes(
            r#"
function plain(tag: Int) -> Int {
    return tag
}

@Main
function main() {
    let a = plain(tag: 1) { 1 }
    print(a)
    return
}
"#,
        ),
        vec!["KSEM233"]
    );
}

#[test]
fn an_extends_clause_is_refused_as_not_yet_executable() {
    assert_eq!(
        library_codes(
            r#"
construct Base {
    let node: Int { 0 }
}

construct Derived extends Base {
    let node: Int { 1 }
}
"#,
        ),
        vec!["KSEM203"]
    );
}

/// A `body { … }` shorthand returns the heterogeneous family value, and the
/// inherited family method dispatches through it.
#[test]
fn a_body_shorthand_and_family_dispatch_check_cleanly() {
    assert!(
        codes(
            r#"
construct Widget {
    @Required let body: Widget
    function value() -> Int {
        return body.value()
    }
}

Widget Leaf(number: Int) {
    function value() -> Int {
        return number
    }
}

Widget Wrapper() {
    body {
        Leaf(number = 7)
    }
}

@Main function main() {
    print(Wrapper().value())
    return
}
"#,
        )
        .is_empty()
    );
}

#[test]
fn an_unknown_construction_input_label_is_refused() {
    assert!(
        codes(
            r#"
construct Shape {
    let area: Int { 0 }
}

Shape Square(side: Int) {
    let area: Int { side }
}

@Main
function main() {
    print(Square(length: 5).area)
    return
}
"#,
        )
        // The unknown label leaves the `side` input unfilled, so a
        // missing-input diagnostic follows the label refusal.
        .contains(&"KSEM204")
    );
}

#[test]
fn a_construction_input_type_mismatch_is_refused() {
    assert_eq!(
        codes(
            r#"
construct Shape {
    let area: Int { 0 }
}

Shape Square(side: Int) {
    let area: Int { side }
}

@Main
function main() {
    print(Square(side: "wide").area)
    return
}
"#,
        ),
        vec!["KSEM207"]
    );
}
