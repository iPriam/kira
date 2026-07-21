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
fn a_content_slot_is_refused_as_not_yet_executable() {
    assert_eq!(
        library_codes(
            r#"
construct Family {
    let node: Int { 0 }
}

Family Stack() {
    @Content let children: [Int]
    let node: Int { 0 }
}
"#,
        ),
        vec!["KSEM203"]
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
