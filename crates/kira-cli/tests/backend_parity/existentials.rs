//! Parity for `some X` / `[some X]`, the Construct 2.0 existential over a
//! construct family.
//!
//! `some X` resolves to the same heterogeneous family value bare `X` does, so
//! what these prove is that the *value* behaves identically on vm, llvm, and
//! hybrid wherever the existential is written: a parameter, a return type, an
//! array element, a struct field, and an enum payload.
//!
//! Reading a `@Required let` off a family value is the piece worth watching.
//! The family states a value obligation; each backed declaration discharges it
//! with either a computed member or a stored field, and the read dispatches on
//! the tag to whichever that declaration chose. A backend that guessed one
//! shape for every variant would return another variant's answer, which is why
//! both shapes appear below rather than only the common one.

use crate::assert_parity;

/// The shape the oracle's stress harness uses: `some X` returned, taken as a
/// parameter, and collected into a `[some X]` local, with a `@Required let`
/// read back through the family value.
///
/// Differentially checked against the oracle's installed 1.7.3 `kira`, which
/// prints the same `9` and `15`.
#[test]
fn an_existential_flows_through_returns_parameters_and_arrays() {
    let output = assert_parity(
        r#"
construct TreeNode {
    @Required let label: String
    @Required let weight: Int
}

TreeNode LeafNode {
    let text: String = ""
    let n: Int = 0
    let label: String { text }
    let weight: Int { n }
}

TreeNode PairNode {
    @Required let first: Any TreeNode
    @Required let second: Any TreeNode
    let label: String { "pair" }
    let weight: Int { 3 }
}

// `some X` in return position, with both variants reachable.
function buildRow(index: Int) -> some TreeNode {
    if index - (index / 2) * 2 == 0 {
        return LeafNode(text: "leaf", n: index)
    }
    return PairNode(first: LeafNode(text: "a", n: 1), second: LeafNode(text: "b", n: 2))
}

// `some X` in parameter position.
function scoreOf(node: borrow Any TreeNode) -> Int {
    if node.label == "leaf" {
        return 1
    }
    return 2
}

@Main
function main() {
    var nodes: [some TreeNode] = []
    var i = 0
    while i < 6 {
        nodes.append(buildRow(i))
        i = i + 1
    }
    var total = 0
    var weight = 0
    for node in nodes {
        total = total + scoreOf(node)
        weight = weight + node.weight
    }
    print(total)
    print(weight)
    return
}
"#,
    );
    assert_eq!(output, "9\n15\n");
}

/// A `@Required let` satisfied by a *stored field* on one declaration and a
/// *computed member* on another dispatches to the right one per variant.
///
/// The two variants deliberately disagree on both the shape and the value, so a
/// dispatcher that fell through to a neighbouring arm — or that assumed one
/// shape for the whole family — prints the wrong number instead of failing
/// loudly.
#[test]
fn a_required_value_member_dispatches_across_field_and_computed_shapes() {
    let output = assert_parity(
        r#"
construct Cell {
    @Required let size: Int
    @Required let tag: String
}

// Discharges both requirements with stored fields.
Cell Stored {
    let size: Int = 11
    let tag: String = "stored"
}

// Discharges both with computed members.
Cell Computed {
    let side: Int = 5
    let size: Int { side * side }
    let tag: String { "computed" }
}

// Mixes the two shapes in one declaration.
Cell Mixed {
    let size: Int = 7
    let tag: String { "mixed" }
}

function describe(cell: borrow Any Cell) -> String {
    return cell.tag
}

@Main
function main() {
    var cells: [some Cell] = []
    cells.append(Stored())
    cells.append(Computed())
    cells.append(Mixed())
    for cell in cells {
        print(describe(cell))
        print(cell.size)
    }
    return
}
"#,
    );
    assert_eq!(output, "stored\n11\ncomputed\n25\nmixed\n7\n");
}

/// `some X` as a struct field and as an enum payload, carried through a move
/// and read back.
///
/// Both are positions the harness writes and neither had a grammar before, so
/// each is exercised end to end rather than merely parsed.
#[test]
fn an_existential_is_carried_by_a_struct_field_and_an_enum_payload() {
    let output = assert_parity(
        r#"
construct Shape {
    @Required let area: Int
}

Shape Circle {
    let r: Int = 0
    let area: Int { r * r * 3 }
}

Shape Square {
    let s: Int = 0
    let area: Int { s * s }
}

struct Holder {
    var node: some Shape
    var count: Int = 0
}

enum Cell {
    Empty
    Filled(some Shape)
}

function areaOf(cell: borrow Cell) -> Int {
    match cell {
        Empty -> { return 0 }
        Filled(shape) -> { return shape.area }
    }
    return 0
}

@Main
function main() {
    let holder = Holder { node: Circle(r: 2), count: 4 }
    print(holder.node.area)
    print(holder.count)
    let cells: [Cell] = [.Filled(Circle(r: 3)), .Empty, .Filled(Square(s: 4))]
    var total = 0
    for cell in cells {
        total = total + areaOf(cell)
    }
    print(total)
    return
}
"#,
    );
    assert_eq!(output, "12\n4\n43\n");
}
