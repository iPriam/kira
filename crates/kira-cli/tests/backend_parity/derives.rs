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
