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
