//! Child slots, trailing content blocks, and builder items on a
//! construct-backed declaration: how caller-provided children are declared
//! (`some X` / `[some X]`), filled, and refused.

use crate::tests::{codes, library_codes};

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
    @Content let children: [Any Family]
    function value() -> Int { return children.count }
}
"#,
        )
        .is_empty()
    );
}

/// `some` names an existential over a *construct family*, so a slot written
/// over a plain struct is refused — in slot position exactly as in a parameter,
/// which is the rule the oracle enforces with one message for both.
#[test]
fn a_child_slot_over_a_non_construct_is_refused() {
    assert_eq!(
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
    let node: Int { 0 }
}
"#,
        ),
        vec!["KSEM237"]
    );
}

/// A child slot over a family type is a real field and checks clean, single and
/// list alike.
#[test]
fn a_family_child_slot_checks_clean() {
    assert!(
        library_codes(
            r#"
construct Child {
    @Required let value: Int
}

Child Leaf {
    let value: Int = 0
}

construct Family {
    let node: Int { 0 }
}

Family One() {
    let child: some Child
    let node: Int { child.value }
}

Family Many() {
    let items: [some Child]
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
construct Child {
    @Required let value: Int
}

Child Leaf {
    let value: Int = 0
}

construct Family {
    let count: Int { 0 }
}

Family One() {
    let child: some Child
    let count: Int { 1 }
}

Family Many() {
    let items: [some Child]
    let count: Int { items.count }
}

@Main
function main() {
    let a = One() { Leaf(value: 3) }
    print(a.count)
    let b = Many() { Leaf(value: 1) Leaf(value: 2) }
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
construct Child {
    @Required let value: Int
}

Child Leaf {
    let value: Int = 0
}

construct Family {
    let count: Int { 0 }
}

Family One() {
    let child: some Child
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
construct Child {
    @Required let value: Int
}

Child Leaf {
    let value: Int = 0
}

construct Family {
    let count: Int { 0 }
}

Family One() {
    let child: some Child
    let count: Int { 1 }
}

@Main
function main() {
    let a = One() { Leaf(value: 1) Leaf(value: 2) }
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

/// A `For`/`if` builder filling a `[some X]` slot type-checks cleanly.
#[test]
fn builder_content_items_check_clean() {
    assert!(
        codes(
            r#"
construct Widget {
    @Required let body: Any Widget
    function total() -> Int { return body.total() }
}

Widget Leaf(number: Int) {
    function total() -> Int { return number }
}

Widget Group() {
    let children: [some Widget]
    function total() -> Int { return 0 }
}

function counts() -> [Int] {
    let xs: [Int] = []
    return xs
}

@Main function main() {
    let on = true
    let g = Group() {
        Leaf(number = 1)
        For(n in counts()) {
            Leaf(number = n)
        }
        if on {
            Leaf(number = 2)
        }
    }
    print(g.total())
    return
}
"#,
        )
        .is_empty()
    );
}

/// A builder's produced child is still checked against the slot's element type.
#[test]
fn a_wrong_typed_builder_child_is_refused() {
    assert!(
        codes(
            r#"
construct Widget {
    @Required let body: Any Widget
    function total() -> Int { return body.total() }
}

Widget Leaf(number: Int) {
    function total() -> Int { return number }
}

Widget Group() {
    let children: [some Widget]
    function total() -> Int { return 0 }
}

function counts() -> [Int] {
    let xs: [Int] = []
    return xs
}

@Main function main() {
    let g = Group() {
        For(n in counts()) {
            n
        }
    }
    print(g.total())
    return
}
"#,
        )
        .iter()
        .any(|code| code == "KSEM232")
    );
}

/// A builder cannot fill a single (`some X`) slot, which takes exactly one
/// child.
#[test]
fn a_builder_filling_a_single_slot_is_refused() {
    assert!(
        codes(
            r#"
construct Widget {
    @Required let body: Any Widget
    function total() -> Int { return body.total() }
}

Widget Leaf(number: Int) {
    function total() -> Int { return number }
}

Widget Wrap() {
    let child: some Widget
    function total() -> Int { return child.total() }
}

function counts() -> [Int] {
    let xs: [Int] = []
    return xs
}

@Main function main() {
    let w = Wrap() {
        For(n in counts()) {
            Leaf(number = n)
        }
    }
    print(w.total())
    return
}
"#,
        )
        .iter()
        .any(|code| code == "KSEM242")
    );
}

/// The same refusal applies wherever `some` is written, not only in slot
/// position: a parameter, a return type, an array element, and a local
/// annotation each name a construct family or are refused.
#[test]
fn some_over_a_non_construct_is_refused_in_every_type_position() {
    let cases = [
        "function f(w: some Leaf) -> Int { return 0 }",
        "function f() -> some Leaf { return Leaf {} }",
        "function f(w: [some Leaf]) -> Int { return 0 }",
        "function f() -> Int { let w: some Leaf = Leaf {} return 0 }",
        "function f(w: some Int) -> Int { return 0 }",
    ];
    for case in cases {
        let source = format!("struct Leaf {{ var value: Int = 0 }}\n{case}\n");
        assert!(
            library_codes(&source).iter().any(|code| code == "KSEM237"),
            "`{case}` was not refused: {:?}",
            library_codes(&source)
        );
    }
}

/// `some Family` and `Any Family` resolve to the same type, so the two
/// spellings are interchangeable in a signature.
#[test]
fn some_family_and_any_family_are_the_same_type() {
    assert!(
        library_codes(
            r#"
construct Family {
    @Required let value: Int
}

Family One {
    let value: Int = 1
}

function takesSome(f: borrow some Family) -> Int {
    return f.value
}

function takesAny(f: borrow Any Family) -> Int {
    return takesSome(f)
}

function roundTrip() -> Int {
    return takesAny(One())
}
"#,
        )
        .is_empty()
    );
}

/// The bare family name is not a type.
///
/// A family is not one of its own values, and the two spellings that *are*
/// types both say which: `Any Family` and `some Family` name a value of some
/// declaration backing it. Left accepted, the bare name reads like a concrete
/// type and hides that the value is heterogeneous.
#[test]
fn the_bare_family_name_is_not_a_type() {
    let source = r#"
construct Family {
    @Required let value: Int
}

Family One {
    let value: Int = 1
}

function takesBare(f: borrow Family) -> Int {
    return f.value
}
"#;
    assert!(
        library_codes(source).iter().any(|code| code == "KSEM207"),
        "{:?}",
        library_codes(source)
    );
}

/// A declaration with more than one child slot fills the first from its bare
/// content block and the rest by name, written after the block or inside it.
#[test]
fn named_fills_reach_every_child_slot() {
    assert!(
        library_codes(
            r#"
construct Family {
    function value() -> Int { return 0 }
}

Family Leaf(number: Int) {
    function value() -> Int { return number }
}

Family Split(gap: Int = 0) {
    let sidebar: some Family
    let detail: some Family
    function value() -> Int { return sidebar.value() + detail.value() + gap }
}

function build() -> Any Family {
    let after = Split { Leaf(number: 1) } detail: { Leaf(number: 2) }
    let inside = Split { Leaf(number: 3) detail: Leaf(number: 4) }
    let all = Split { } sidebar: { Leaf(number: 5) } detail: Leaf(number: 6)
    return after
}
"#,
        )
        .is_empty()
    );
}

/// The bare content block is the first slot's, so naming that slot as well
/// fills it twice.
#[test]
fn a_child_slot_filled_twice_is_refused() {
    assert_eq!(
        library_codes(
            r#"
construct Family {
    function value() -> Int { return 0 }
}

Family Leaf(number: Int) {
    function value() -> Int { return number }
}

Family Split() {
    let sidebar: some Family
    let detail: some Family
    function value() -> Int { return sidebar.value() + detail.value() }
}

function build() -> Any Family {
    return Split { Leaf(number: 1) } sidebar: { Leaf(number: 2) } detail: { Leaf(number: 3) }
}
"#,
        ),
        vec!["KSEM274"]
    );
}

/// A `{ … }` content block fills a slot by name; it is not a value, so it is
/// refused anywhere a value is expected.
#[test]
fn a_content_block_is_not_a_value() {
    assert_eq!(
        library_codes(
            r#"
construct Family {
    function value() -> Int { return 0 }
}

Family Leaf(number: Int) {
    function value() -> Int { return number }
}

Family Split() {
    let sidebar: some Family
    let detail: some Family
    function value() -> Int { return sidebar.value() + detail.value() }
}

function build() -> Any Family {
    return Split { Leaf(number: 1) } gap: { Leaf(number: 2) }
}
"#,
        ),
        vec!["KSEM273", "KSEM204", "KSEM208"]
    );
}

/// A single slot nobody filled and with no declared default is a missing
/// construction input, reported the way any other unfilled field is.
#[test]
fn an_unfilled_child_slot_without_a_default_is_missing() {
    assert_eq!(
        library_codes(
            r#"
construct Family {
    function value() -> Int { return 0 }
}

Family Leaf(number: Int) {
    function value() -> Int { return number }
}

Family Split() {
    let sidebar: some Family
    let detail: some Family
    function value() -> Int { return sidebar.value() + detail.value() }
}

function build() -> Any Family {
    return Split { Leaf(number: 1) }
}
"#,
        ),
        vec!["KSEM208"]
    );
}

/// A child slot may declare a default, which stands in for the slot nobody
/// filled.
#[test]
fn a_child_slot_default_stands_in_for_an_unfilled_slot() {
    assert!(
        library_codes(
            r#"
construct Family {
    function value() -> Int { return 0 }
}

Family Leaf(number: Int) {
    function value() -> Int { return number }
}

Family Split() {
    let sidebar: some Family
    let detail: some Family = Leaf(number: 9)
    function value() -> Int { return sidebar.value() + detail.value() }
}

function build() -> Any Family {
    return Split { Leaf(number: 1) }
}
"#,
        )
        .is_empty()
    );
}

/// A named fill is checked against its slot's type, the way a child written in
/// the block is.
#[test]
fn a_named_fill_of_the_wrong_type_is_refused() {
    assert_eq!(
        library_codes(
            r#"
construct Family {
    function value() -> Int { return 0 }
}

Family Leaf(number: Int) {
    function value() -> Int { return number }
}

Family Split() {
    let sidebar: some Family
    let detail: some Family
    function value() -> Int { return sidebar.value() + detail.value() }
}

function build() -> Any Family {
    return Split { Leaf(number: 1) } detail: 7
}
"#,
        ),
        vec!["KSEM232"]
    );
}

/// A child slot declared by a construct family is a slot of every declaration
/// backed by it, filled at a construction the same way an own slot is.
#[test]
fn an_inherited_family_child_slot_is_filled_like_an_own_slot() {
    assert!(
        library_codes(
            r#"
construct Family {
    let children: [some Family]
    function value() -> Int { return children.count }
}

Family Leaf(number: Int) {
    function value() -> Int { return number }
}

Family Wrap() {
    function value() -> Int { return children.count * 2 }
}

function build() -> Any Family {
    return Wrap { Leaf(number: 1) Leaf(number: 2) }
}
"#,
        )
        .is_empty()
    );
}
