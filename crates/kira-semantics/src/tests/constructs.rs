//! Semantic analysis of the construct declaration family: a family template, a
//! construct-backed declaration that conforms to one, its construction, and the
//! read of its computed bridge member — plus every typed refusal.
//!
//! Two neighbouring surfaces have their own files: [`requirements`] covers what
//! a family obliges its declarations to provide, and [`slots`] covers the
//! children a construction passes.

mod requirements;
mod slots;

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

/// An `extend` block adds a fluent modifier the family value and a concrete
/// backed value both call, and a chain of them type-checks clean.
#[test]
fn an_extend_modifier_is_callable_on_a_family_and_a_concrete_value() {
    assert!(
        codes(
            r#"
construct Widget {
    let tag: Int { 0 }
}

Widget Leaf(id: Int) {
    let tag: Int { id }
}

Widget Padding(length: Int) {
    let child: some Widget
    let tag: Int { 0 }
}

extend Widget {
    function padding(length: Int) -> Widget {
        return Padding(length: length) {
            self
        }
    }
}

@Main
function main() {
    let base = Leaf(id: 1)
    let once = base.padding(8)
    let twice = once.padding(4)
    print(twice.tag)
    return
}
"#,
        )
        .is_empty()
    );
}

/// Extending a family that does not exist is refused.
#[test]
fn extending_an_unknown_family_is_refused() {
    assert!(
        library_codes(
            r#"
extend Gadget {
    function noop() -> Int {
        return 0
    }
}
"#,
        )
        .contains(&"KSEM238")
    );
}

/// A modifier whose name is already a family method is refused rather than
/// shadowing it.
#[test]
fn a_modifier_colliding_with_a_family_method_is_refused() {
    assert!(
        codes(
            r#"
construct Widget {
    function lower() -> Int { return 0 }
}

extend Widget {
    function lower() -> Widget {
        return self
    }
}
"#,
        )
        .contains(&"KSEM239")
    );
}

/// A construct-backed declaration names a thing, not a type, so a member is
/// callable on the name itself: `Sprite.draw()` builds the declaration with its
/// declared defaults and calls `draw` on it.
#[test]
fn a_declaration_qualified_call_checks_clean() {
    assert!(
        codes(
            r#"
construct Drawable {
    requires {
        function draw() -> Int
    }
}

Drawable Sprite {
    let base: Int = 7
    function draw() -> Int { return base + Sprite.offset() }
    function offset() -> Int { return 3 }
}

@Main
function main() {
    print(Sprite.draw())
    return
}
"#,
        )
        .is_empty()
    );
}

/// The default construction is real, so an input with no default is reported at
/// the qualified call exactly as a written `Sprite()` would report it.
#[test]
fn a_declaration_qualified_call_needs_every_input_to_have_a_default() {
    assert!(
        codes(
            r#"
construct Boxed {
    @Required let size: Int
}

Boxed Boxy {
    let size: Int = 4
    let scale: Int
    function area() -> Int { return size * scale }
}

@Main
function main() {
    print(Boxy.area())
    return
}
"#,
        )
        .contains(&"KSEM208")
    );
}

/// A member the declaration does not have is reported on the declaration, not
/// swallowed into the class parent-qualifier rule.
#[test]
fn a_declaration_qualified_call_to_a_missing_member_is_reported() {
    assert!(
        codes(
            r#"
construct Drawable {
    requires {
        function draw() -> Int
    }
}

Drawable Sprite {
    function draw() -> Int { return 1 }
}

@Main
function main() {
    print(Sprite.missing())
    return
}
"#,
        )
        .contains(&"KSEM097")
    );
}

/// A plain `class` gets none of this: its name is a type, and a type has no
/// value of its own to run a member against.
#[test]
fn a_class_name_is_still_not_callable_without_an_instance() {
    assert!(
        codes(
            r#"
class Account {
    let rate: Int = 2
    function gross() -> Int { return self.rate * 10 }
}

@Main
function main() {
    print(Account.gross())
    return
}
"#,
        )
        .contains(&"KSEM069")
    );
}
