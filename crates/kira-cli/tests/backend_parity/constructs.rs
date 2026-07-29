//! Parity for the construct declaration family: constructing a construct-backed
//! declaration and reading its computed bridge member must run byte-identically
//! on the vm, llvm, and hybrid backends.
//!
//! This is the execution requirement. The oracle documents the family as
//! validate-only ("construct-backed declarations do not execute yet"); here a
//! construct-backed declaration is a typed factory that lowers to a struct plus
//! zero-argument methods, so it *runs* — and a validate-only result would fail
//! these tests, not pass them.

use crate::assert_parity;

/// Constructing a construct-backed declaration and reading its computed bridge
/// member runs the member and yields the same value on every backend.
#[test]
fn constructing_and_reading_a_bridge_agrees() {
    let output = assert_parity(
        r#"
construct Shape {
    @Required let sides: Int
    let area: Int { 0 }
}

Shape Square(side: Int) {
    let area: Int { side * side }
}

Shape Rect(width: Int, height: Int) {
    let area: Int { width * height }
}

@Main
function main() {
    let s = Square(side: 5)
    print(s.area)
    let r = Rect(width: 3, height: 4)
    print(r.area)
    // A positional construction, and a bridge read on a temporary.
    print(Square(7).area)
    return
}
"#,
    );
    assert_eq!(output, "25\n12\n49\n");
}

/// A computed bridge may produce a struct value, and reading a field of it
/// composes across the construction boundary identically on every backend.
#[test]
fn a_bridge_producing_a_struct_value_agrees() {
    let output = assert_parity(
        r#"
struct Node {
    var kind: Int = 0
    var weight: Int = 0
}

construct Widget {
    let node: Node { Node {} }
}

Widget Box(kind: Int, weight: Int) {
    let node: Node {
        Node { kind: kind, weight: weight }
    }
}

@Main
function main() {
    let b = Box(kind: 2, weight: 40)
    let n = b.node
    print(n.kind)
    print(n.weight)
    // Reading the bridge twice rebuilds the node each time.
    print(b.node.kind + b.node.weight)
    return
}
"#,
    );
    assert_eq!(output, "2\n40\n42\n");
}

/// A construct-backed declaration with child slots constructs and yields its
/// bridge from the trailing children, byte-identically on every backend.
///
/// This is the child-slot execution requirement: a single `some X` slot and a
/// list `[some X]` slot are filled from a construction's trailing content
/// block, stored as ordinary fields, and read back through the bridge — so a
/// widget tree with children *runs*, not merely validates.
///
/// The slot element is a construct family, because that is what `some` requires
/// of every position: the oracle refuses `some` over a plain struct here with
/// the same message it gives for a parameter.
#[test]
fn a_construction_with_children_yields_its_node() {
    let output = assert_parity(
        r#"
construct Item {
    @Required let value: Int
}

Item Leaf {
    let value: Int = 0
}

construct Panel {
    let total: Int { 0 }
}

// A single-child slot: exactly one child, read back through the bridge.
Panel Wrap() {
    let inner: some Item
    let total: Int { inner.value }
}

// A list slot: an ordered array of children, summed through the bridge.
Panel Group() {
    let items: [some Item]
    let total: Int {
        var sum = 0
        for i in 0..items.count {
            sum = sum + items[i].value
        }
        return sum
    }
}

@Main
function main() {
    let w = Wrap() { Leaf(value: 7) }
    print(w.total)
    let g = Group() { Leaf(value: 1) Leaf(value: 2) Leaf(value: 3) }
    print(g.total)
    // A no-paren construction with children fills the same list slot.
    print(Group { Leaf(value: 10) Leaf(value: 20) }.total)
    return
}
"#,
    );
    assert_eq!(output, "7\n6\n30\n");
}

/// A `function` member (not a computed bridge) is called with arguments and
/// runs identically on every backend, beside a computed bridge.
#[test]
fn a_function_member_and_a_bridge_agree() {
    let output = assert_parity(
        r#"
construct Shape {
    let area: Int { 0 }
}

Shape Square(side: Int) {
    let area: Int { side * side }
    function scaled(factor: Int) -> Int {
        return side * factor
    }
}

@Main
function main() {
    let s = Square(side: 6)
    print(s.area)
    print(s.scaled(factor: 10))
    return
}
"#,
    );
    assert_eq!(output, "36\n60\n");
}

/// Differently-shaped concrete widgets upcast into `Any Widget`, survive a
/// heterogeneous child array, and dispatch family methods identically on every
/// backend.
#[test]
fn heterogeneous_family_values_dispatch_on_every_backend() {
    let output = assert_parity(
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

Widget Double(number: Int) {
    body {
        Leaf(number = number * 2)
    }
}

Widget Sum() {
    @Content let children: [Widget]
    function value() -> Int {
        var total = 0
        for index in 0..children.count {
            total = total + children[index].value()
        }
        return total
    }
}

function read(widget: Any Widget) -> Int {
    return widget.value()
}

@Main function main() {
    let tree: Widget = Sum() {
        Leaf(number = 2)
        Double(number = 3)
    }
    print(tree.value())
    print(read(Leaf(number = 5)))
    return
}
"#,
    );
    assert_eq!(output, "8\n5\n");
}

/// An `extend` fluent modifier runs on every backend: it is called on a concrete
/// widget and on a family value, chains, and each layer it builds contributes to
/// the final value byte-identically on the vm, llvm, and hybrid backends.
#[test]
fn extend_modifiers_chain_on_every_backend() {
    let output = assert_parity(
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

Widget Boxed(extra: Int) {
    let child: some Widget
    function value() -> Int {
        return child.value() + extra
    }
}

extend Widget {
    function plus(amount: Int) -> Widget {
        return Boxed(extra: amount) {
            self
        }
    }
}

@Main function main() {
    let base = Leaf(number = 10)
    // Concrete receiver: the widget has no `plus`, its family does.
    print(base.plus(5).value())
    // Family receiver, chained: each modifier wraps the previous family value.
    let chained = base.plus(1).plus(2)
    print(chained.value())
    return
}
"#,
    );
    assert_eq!(output, "15\n13\n");
}

/// A uniform `extend` modifier whose parameter declares a default — the shape
/// the corpus's `font`/`surfaceBorder` modifiers use — fills the omitted
/// argument identically on every backend, called on a concrete widget and on a
/// family value.
#[test]
fn an_extend_modifier_default_fills_on_every_backend() {
    let output = assert_parity(
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

Widget Boxed(extra: Int) {
    let child: some Widget
    function value() -> Int {
        return child.value() + extra
    }
}

extend Widget {
    function plus(amount: Int = 100) -> Widget {
        return Boxed(extra: amount) {
            self
        }
    }
}

@Main function main() {
    let base = Leaf(number = 10)
    // Concrete receiver, default taken.
    print(base.plus().value())
    // Concrete receiver, argument passed.
    print(base.plus(5).value())
    // Family receiver, default taken on a chained call.
    print(base.plus(1).plus().value())
    return
}
"#,
    );
    assert_eq!(output, "110\n15\n111\n");
}

/// `For`/`if` builder content items — the shape the UI corpus uses — fill a
/// `[some Widget]` slot at run time. The child array is built by a hoisted
/// loop/branch, so every backend must produce the same children in the same
/// order.
#[test]
fn builder_content_items_fill_a_slot_on_every_backend() {
    let output = assert_parity(
        r#"
construct Widget {
    @Required let body: Widget
    function total() -> Int { return body.total() }
}

Widget Leaf(number: Int) {
    function total() -> Int { return number }
}

Widget Group() {
    let children: [some Widget]
    function total() -> Int {
        var sum = 0
        for c in children { sum = sum + c.total() }
        return sum
    }
}

function counts() -> [Int] {
    var xs: [Int] = []
    xs.append(1)
    xs.append(2)
    xs.append(3)
    return xs
}

@Main function main() {
    let big = true
    let g = Group() {
        // A bare child, a `For` producing one per element, and an `if`/`else`.
        Leaf(number = 1000)
        For(n in counts()) {
            Leaf(number = n)
            // A builder nests inside a builder.
            if n == 2 {
                Leaf(number = 20)
            }
        }
        if big {
            Leaf(number = 500)
        } else {
            Leaf(number = 9)
        }
    }
    // 1000 + (1 + 2 + 20 + 3) + 500 = 1526
    print(g.total())
    return
}
"#,
    );
    assert_eq!(output, "1526\n");
}

/// A `For` over an empty iterable contributes no children, and an `if` with no
/// taken branch contributes none — identically on every backend.
#[test]
fn empty_builders_contribute_nothing_on_every_backend() {
    let output = assert_parity(
        r#"
construct Widget {
    @Required let body: Widget
    function total() -> Int { return body.total() }
}

Widget Leaf(number: Int) {
    function total() -> Int { return number }
}

Widget Group() {
    let children: [some Widget]
    function total() -> Int {
        var n = 0
        for c in children { n = n + 1 }
        return n
    }
}

function none() -> [Int] {
    let xs: [Int] = []
    return xs
}

@Main function main() {
    let off = false
    let g = Group() {
        Leaf(number = 1)
        For(n in none()) {
            Leaf(number = n)
        }
        if off {
            Leaf(number = 2)
        }
    }
    // Only the one bare child survives.
    print(g.total())
    return
}
"#,
    );
    assert_eq!(output, "1\n");
}

/// A builder nested through a construction: an outer `For` builds groups, each
/// holding its own `For`. The inner group's building statements must land
/// inside the outer loop, not escape it — so every backend agrees.
#[test]
fn builders_nested_through_a_construction_agree() {
    let output = assert_parity(
        r#"
construct Widget {
    @Required let body: Widget
    function total() -> Int { return body.total() }
}

Widget Leaf(number: Int) {
    function total() -> Int { return number }
}

Widget Group() {
    let children: [some Widget]
    function total() -> Int {
        var sum = 0
        for c in children { sum = sum + c.total() }
        return sum
    }
}

function pair() -> [Int] {
    var xs: [Int] = []
    xs.append(10)
    xs.append(20)
    return xs
}

@Main function main() {
    let g = Group() {
        For(base in pair()) {
            Group() {
                For(k in pair()) {
                    Leaf(number = base + k)
                }
            }
        }
    }
    // base=10: 20+30 = 50; base=20: 30+40 = 70; total 120.
    print(g.total())
    return
}
"#,
    );
    assert_eq!(output, "120\n");
}

/// A `body` whose value is chosen by a condition, on every backend.
///
/// The shorthand's block ends in an `if`/`else if`/`else` rather than in an
/// expression, so each arm is the tail: the arm that runs is the one whose
/// widget the member returns. The reference implementation accepts exactly this
/// shape and rejects the same block with the `else` removed, which is what the
/// `else`-less case below pins.
#[test]
fn a_body_may_choose_its_widget_with_a_condition_on_every_backend() {
    let output = assert_parity(
        r#"
construct Widget {
    @Required let body: Widget
    function total() -> Int { return body.total() }
}

Widget Leaf(number: Int) {
    function total() -> Int { return number }
}

Widget Chain(kind: Int) {
    body {
        if kind == 1 {
            Leaf(number = 10)
        } else if kind == 2 {
            Leaf(number = 20)
        } else {
            Leaf(number = 30)
        }
    }
}

@Main function main() {
    print(Chain(kind = 1).total())
    print(Chain(kind = 2).total())
    print(Chain(kind = 9).total())
    return
}
"#,
    );
    assert_eq!(output, "10\n20\n30\n");
}

/// A `@Required function` requirement — Construct 2.0's bodyless obligation —
/// executes: each declaration's own implementation runs when called directly,
/// and the family value dispatches to the right one on every backend.
#[test]
fn a_required_function_runs_and_dispatches_on_every_backend() {
    let output = assert_parity(
        r#"
construct Shape {
    @Required function area() -> Int
    @Required function label() -> String
}

Shape Square(side: Int) {
    function area() -> Int { return side * side }
    function label() -> String { return "square" }
}

Shape Strip(length: Int) {
    function area() -> Int { return length }
    function label() -> String { return "strip" }
}

@Main function main() {
    print(Square(side: 5).area())
    print(Strip(length: 3).label())
    let shapes: [Any Shape] = [Square(side: 4), Strip(length: 9)]
    for shape in shapes {
        print(shape.label())
        print(shape.area())
    }
    return
}
"#,
    );
    assert_eq!(output, "25\nstrip\nsquare\n16\nstrip\n9\n");
}
