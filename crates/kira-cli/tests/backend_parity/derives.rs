//! Parity for the builtin derives: Foundation's four macros, and the compiler's
//! own `@Derive(Copy)` assertion.
//!
//! These sit apart from `macros.rs` because what they prove is different. The
//! macro tests prove the *mechanism*; these prove the four functions Foundation
//! generates behave identically on every backend, down to the exact wire string
//! the serde pair writes and the trap malformed input produces.

use crate::assert_parity;

/// Foundation's `@Derive(Equatable)` and `@Derive(Clone)`, reached through
/// `import Foundation` and recursing through a nested derived struct.
#[test]
fn the_foundation_equality_and_clone_derives_agree() {
    let output = assert_parity(
        r#"
import Foundation

@Derive(Equatable, Clone)
struct DrvPoint {
    var x: Int
    var y: Int
}

@Derive(Equatable, Clone)
struct DrvSegment {
    var from: DrvPoint
    var to: DrvPoint
    var label: String
}

@Main
function main() {
    let origin = DrvPoint { x: 1, y: 2 }
    let copied = clone_DrvPoint(origin)
    print(eq_DrvPoint(origin, copied))
    print(eq_DrvPoint(origin, DrvPoint { x: 9, y: 2 }))
    let segment = DrvSegment { from: origin, to: DrvPoint { x: 3, y: 4 }, label: "edge" }
    let twin = clone_DrvSegment(segment)
    print(eq_DrvSegment(segment, twin))
    print(twin.label)
    return
}
"#,
    );
    assert_eq!(output, "true\nfalse\ntrue\nedge\n");
}

/// Foundation's `@Derive(Serializable)` / `@Derive(Deserializable)`: the exact
/// wire string, and the round-trip law asserted with `eq_`.
#[test]
fn the_foundation_serde_derives_agree() {
    let output = assert_parity(
        r#"
import Foundation

@Derive(Equatable, Serializable, Deserializable)
struct DsxPoint {
    var x: Int
    var y: Int
}

@Derive(Equatable, Serializable, Deserializable)
struct DsxSegment {
    var from: DsxPoint
    var to: DsxPoint
    var label: String
    var live: Bool
}

@Main
function main() {
    let point = DsxPoint { x: 1, y: -2 }
    let wire = serialize_DsxPoint(point)
    print(wire)
    print(eq_DsxPoint(point, deserialize_DsxPoint(wire)))
    print(eq_DsxPoint(point, deserialize_DsxPoint("DsxPoint{x=1;y=-2}")))

    let segment = DsxSegment {
        from: point,
        to: DsxPoint { x: 3, y: 4 },
        label: "edge",
        live: true
    }
    let nested = serialize_DsxSegment(segment)
    print(nested)
    print(eq_DsxSegment(segment, deserialize_DsxSegment(nested)))

    let empty = DsxSegment { from: point, to: point, label: "", live: false }
    print(serialize_DsxSegment(empty))
    print(eq_DsxSegment(empty, deserialize_DsxSegment(serialize_DsxSegment(empty))))
    return
}
"#,
    );
    assert_eq!(
        output,
        "DsxPoint{x=1;y=-2}\n\
         true\n\
         true\n\
         DsxSegment{from=DsxPoint{x=1;y=-2};to=DsxPoint{x=3;y=4};label=\"edge\";live=true}\n\
         true\n\
         DsxSegment{from=DsxPoint{x=1;y=-2};to=DsxPoint{x=1;y=-2};label=\"\";live=false}\n\
         true\n"
    );
}

/// An enum serializes by VARIANT NAME, carries its payload in parentheses, and
/// round-trips inside a struct field — on every backend.
///
/// The name is the contract: an ordinal would tie the wire format to
/// declaration order, so inserting a variant would re-read every stored value
/// as its neighbour.
#[test]
fn the_foundation_serde_derives_agree_over_enums() {
    let output = assert_parity(
        r#"
import Foundation

@Derive(Equatable, Serializable, Deserializable)
enum DsxTool { Select Move Rotate }

@Derive(Equatable, Serializable, Deserializable)
enum DsxNote { Blank Rank(Int) Tag(String) }

@Derive(Equatable, Serializable, Deserializable)
struct DsxSlot {
    var tool: DsxTool
    var note: DsxNote
    var count: Int
}

@Main
function main() {
    print(serialize_DsxTool(DsxTool.Rotate))
    print(serialize_DsxNote(DsxNote.Blank))
    print(serialize_DsxNote(DsxNote.Rank(-7)))
    print(serialize_DsxNote(DsxNote.Tag("edge")))

    print(eq_DsxTool(DsxTool.Move, deserialize_DsxTool("DsxTool.Move")))
    print(eq_DsxNote(DsxNote.Rank(42), deserialize_DsxNote(serialize_DsxNote(DsxNote.Rank(42)))))
    print(eq_DsxNote(DsxNote.Tag("x"), deserialize_DsxNote(serialize_DsxNote(DsxNote.Tag("x")))))

    let slot = DsxSlot { tool: DsxTool.Select, note: DsxNote.Tag("hi"), count: 3 }
    let wire = serialize_DsxSlot(slot)
    print(wire)
    print(eq_DsxSlot(slot, deserialize_DsxSlot(wire)))
    return
}
"#,
    );
    assert_eq!(
        output,
        "DsxTool.Rotate\n\
         DsxNote.Blank\n\
         DsxNote.Rank(-7)\n\
         DsxNote.Tag(\"edge\")\n\
         true\n\
         true\n\
         true\n\
         DsxSlot{tool=DsxTool.Select;note=DsxNote.Tag(\"hi\");count=3}\n\
         true\n"
    );
}

/// Malformed wire text traps on every backend rather than parsing partially.
#[test]
fn malformed_serialized_text_traps_on_every_backend() {
    for wire in [
        "garbage",
        "DsxPoint{x=1",
        "Other{x=1;y=2}",
        "DsxPoint{x=n;y=2}",
    ] {
        let source = format!(
            r#"
import Foundation

@Derive(Deserializable)
struct DsxPoint {{
    var x: Int
    var y: Int
}}

@Main
function main() {{
    let back = deserialize_DsxPoint("{wire}")
    print(back.x)
    return
}}
"#
        );
        // Every backend agrees, and what they agree on is producing no output
        // and failing: `assert_parity` compares stdout and exit status, so this
        // is the trap being identical rather than merely present.
        let output = assert_parity(&source);
        assert_eq!(output, "", "`{wire}` produced output instead of trapping");
    }
}

/// `@Derive(Copy)` on an eligible type is a no-op: it compiles, the type still
/// copies, and it composes with a Foundation derive.
#[test]
fn the_copy_derive_is_a_no_op_on_an_eligible_type() {
    let output = assert_parity(
        r#"
import Foundation

@Derive(Copy, Equatable)
struct DcxPoint {
    var x: Int
    var y: Int
}

@Derive(Copy)
enum DcxTone {
    Red
    Green
}

function dcxShade(tone: DcxTone) -> Int {
    match tone {
        Red -> return 1
        Green -> return 2
    }
}

@Main
function main() {
    let first = DcxPoint { x: 1, y: 2 }
    // A `Copy` field is read twice with no move, which is the whole claim.
    let second = first
    print(eq_DcxPoint(first, second))
    print(first.x + second.y)
    print(dcxShade(.Green))
    return
}
"#,
    );
    assert_eq!(output, "true\n3\n2\n");
}

/// `@Derive(Tagged)` turns an enum's variants into numbers and back.
///
/// The codes are declaration order, so this pins them literally: a reorder that
/// silently renumbered every artifact already written is exactly what the derive
/// exists to prevent, and a test that only checked a round trip would not see
/// one.
#[test]
fn a_tagged_enum_numbers_its_variants_identically_on_every_backend() {
    let output = assert_parity(
        r#"
import Foundation

@Derive(Tagged)
enum Material {
    Flat
    Frosted
    LiquidGlass
}

@Main
function main() {
    print(code_Material(.Flat))
    print(code_Material(.Frosted))
    print(code_Material(.LiquidGlass))
    // A code names the variant it came from.
    print(code_Material(Material_fromCode(1)))
    // Decoding is total: a number naming no variant answers the first rather
    // than trapping, because a decoder has to be able to survive bad input.
    print(code_Material(Material_fromCode(99)))
    return
}
"#,
    );
    assert_eq!(output, "0\n1\n2\n1\n0\n");
}

/// `@Derive(Hashable)` folds a struct's fields into one number.
///
/// The fold must agree across engines *and* across field kinds — a string
/// folded byte by byte, a bool as a branch, a nested struct through its own
/// derive — because a hash that differs by backend would make a cache built on
/// one wrong on the other.
#[test]
fn a_hashable_struct_folds_identically_on_every_backend() {
    let output = assert_parity(
        r#"
import Foundation

@Derive(Hashable)
struct Inner {
    var tag: Int
}

@Derive(Hashable)
struct Point {
    var x: Int
    var y: Int
    var name: String
    var live: Bool
    var inner: Inner
}

@Main
function main() {
    let a = Point { x: 1, y: 2, name: "p", live: true, inner: Inner { tag: 7 } }
    let same = Point { x: 1, y: 2, name: "p", live: true, inner: Inner { tag: 7 } }
    // Every field is in the fold, so changing any one of them changes it.
    let other_int = Point { x: 1, y: 3, name: "p", live: true, inner: Inner { tag: 7 } }
    let other_text = Point { x: 1, y: 2, name: "q", live: true, inner: Inner { tag: 7 } }
    let other_bool = Point { x: 1, y: 2, name: "p", live: false, inner: Inner { tag: 7 } }
    let other_nested = Point { x: 1, y: 2, name: "p", live: true, inner: Inner { tag: 8 } }
    print(hash_Point(a) == hash_Point(same))
    print(hash_Point(a) == hash_Point(other_int))
    print(hash_Point(a) == hash_Point(other_text))
    print(hash_Point(a) == hash_Point(other_bool))
    print(hash_Point(a) == hash_Point(other_nested))
    return
}
"#,
    );
    assert_eq!(output, "true\nfalse\nfalse\nfalse\nfalse\n");
}
