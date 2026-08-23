//! Backend parity for inferred construct fields, bare braces, and copy/update
//! rebuilding.

use crate::assert_parity;

#[test]
fn inferred_fields_and_nested_updates_execute_identically() {
    let output = assert_parity(
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

construct Base() extends Style {
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
    );
    assert_eq!(output, "8\n13\n");
}

#[test]
fn later_construct_members_read_earlier_members_on_every_backend() {
    let output = assert_parity(
        r#"
construct Theme {
    let value: Int = 0
}

construct Concrete() extends Theme {
    let value = 3
    let doubled = value + 1
}

@Main
function main() {
    print(Concrete {}.doubled)
    return
}
"#,
    );
    assert_eq!(output, "4\n");
}

#[test]
fn bare_braced_constructs_keep_child_content_on_every_backend() {
    let output = assert_parity(
        r#"
construct Child {
    @Required let value: Int
}

construct Leaf(value: Int) extends Child {
    let result: Int { value }
}

construct Stack {
    let child: some Child
    let result: Int { child.value }
}

construct Wrap() extends Stack {
    let child: some Child
}

@Main
function main() {
    let stack = Wrap { Leaf(value: 7) }
    print(stack.result)
    return
}
"#,
    );
    assert_eq!(output, "7\n");
}

/// A family's value members — required and defaulted alike — read through
/// `Any Family` in a free function, where nothing is specialized: every read
/// runs the synthesized tag dispatcher.
#[test]
fn family_value_members_read_through_the_family_value() {
    let output = assert_parity(
        r#"
construct Style {
    @Required let colors: Int
    let appearance: Int = 2
}

construct Base() extends Style {
    let colors = 7
}

construct Light() extends Style {
    let colors = 9
    let appearance = 5
}

function readThrough(style: Any Style) -> Int {
    return style.appearance + style.colors
}

@Main
function main() {
    print(readThrough(Base {}))
    print(readThrough(Light {}))
    return
}
"#,
    );
    assert_eq!(output, "9\n14\n");
}

/// A construction inside an `if` condition hoists its defaulted members as
/// `let`s; they must run before the test, not inside the then-branch after it.
#[test]
fn a_construction_in_an_if_condition_initializes_before_the_test() {
    let output = assert_parity(
        r#"
construct Style {
    @Required let colors: Int
    let appearance: Int = 2
}

construct Base() extends Style {
    let colors = 7
}

function readThrough(style: Any Style) -> Int {
    return style.appearance + style.colors
}

@Main
function main() {
    if readThrough(Base {}) == 9 {
        print("if-cond-ok")
    }
    return
}
"#,
    );
    assert_eq!(output, "if-cond-ok\n");
}

/// A construction inside a `while` condition re-initializes its defaulted
/// members ahead of EVERY test, so the loop neither reads uninitialized locals
/// on the first pass nor stale ones afterwards.
#[test]
fn a_construction_in_a_while_condition_initializes_before_every_test() {
    let output = assert_parity(
        r#"
construct Style {
    @Required let colors: Int
    let appearance: Int = 2
}

construct Base() extends Style {
    let colors = 7
}

function readThrough(style: Any Style) -> Int {
    return style.appearance + style.colors
}

@Main
function main() {
    var laps = 0
    while readThrough(Base {}) == 9 && laps < 3 {
        laps = laps + 1
    }
    print(laps)
    return
}
"#,
    );
    assert_eq!(output, "3\n");
}

/// A function-typed construct parameter mints its function-type struct while
/// the construct's fields resolve — between the pass that mints construct ids
/// and the pass that records their defaults. Every construct declared after it
/// must keep its own defaults rather than inheriting a neighbor's.
#[test]
fn a_function_typed_construct_parameter_leaves_later_defaults_aligned() {
    let output = assert_parity(
        r#"
construct Widget {
    @Required let body: Int
}

construct Sliding(value: Float, onChange: (Float) -> Void) extends Widget {
    let body: Int = 1
}

construct Style {
    @Required let colors: Int
    let appearance: Int = 2
}

construct Base() extends Style {
    let colors = 7
}

function ignoreFloat(value: Float) {
    let ignored = value + 0.0
    return
}

@Main
function main() {
    let style = Base {}
    print(style.colors + style.appearance)
    let s = Sliding(value: 0.5, onChange: ignoreFloat)
    print(s.body)
    return
}
"#,
    );
    assert_eq!(output, "9\n1\n");
}
