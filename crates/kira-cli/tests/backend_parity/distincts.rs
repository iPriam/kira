//! Parity for `distinct Name = Representation`.
//!
//! A distinct type is erased before either backend sees a program, so what
//! these prove is that the erasure leaves *one* program: the VM and the native
//! backend run the same instructions on the same words, and a program written
//! against the distinct type prints exactly what the same program written
//! against the representation prints.

use crate::assert_parity;

/// Construction, `.raw`, equality, and a distinct type in a struct field, an
/// array element, and a parameter — one program covering every position.
#[test]
fn a_distinct_type_runs_identically_on_both_backends() {
    let output = assert_parity(
        r#"
distinct DstTabId = U32
distinct DstBookmarkId = U32

struct DstTab {
    var id: DstTabId
    var title: String
}

function dstIndex(id: DstTabId) -> Int {
    return Int(id.raw)
}

@Main
function main() {
    let first = DstTabId(U32(7))
    let second = DstTabId(U32(9))
    print(dstIndex(first))
    print(first == second)
    print(first == DstTabId(U32(7)))
    let tab = DstTab { id: second, title: "notes" }
    print(dstIndex(tab.id))
    print(tab.title)
    var ids: [DstTabId] = []
    ids.append(first)
    ids.append(second)
    var total: Int = 0
    for id in ids {
        total = total + dstIndex(id)
    }
    print(total)
    let bookmark = DstBookmarkId(U32(7))
    print(Int(bookmark.raw) == dstIndex(first))
    return
}
"#,
    );
    assert_eq!(output, "7\nfalse\ntrue\n9\nnotes\n16\ntrue\n");
}

/// The same program written twice — once against `distinct TabId = U32`, once
/// against a bare `U32` — prints the same thing on every backend.
///
/// This is the zero-cost claim as a program rather than as an assertion about
/// the compiler: if the type cost a wrapper, a box, or a conversion anywhere,
/// the two runs would have to differ somewhere observable.
#[test]
fn a_distinct_type_prints_what_its_representation_prints() {
    let with_distinct = assert_parity(
        r#"
distinct DsxCounter = U32

function dsxStep(value: DsxCounter) -> DsxCounter {
    return DsxCounter(value.raw + U32(1))
}

@Main
function main() {
    var value = DsxCounter(U32(0))
    var index: Int = 0
    while index < 5 {
        value = dsxStep(value)
        index = index + 1
    }
    print(Int(value.raw))
    return
}
"#,
    );
    let with_representation = assert_parity(
        r#"
function dsyStep(value: U32) -> U32 {
    return value + U32(1)
}

@Main
function main() {
    var value = U32(0)
    var index: Int = 0
    while index < 5 {
        value = dsyStep(value)
        index = index + 1
    }
    print(Int(value))
    return
}
"#,
    );
    assert_eq!(with_distinct, with_representation);
    assert_eq!(with_distinct, "5\n");
}

/// Foundation's derives on a distinct type: equality, clone, hash, and a serde
/// round trip, all agreeing on both backends.
#[test]
fn the_foundation_derives_agree_on_a_distinct_type() {
    let output = assert_parity(
        r#"
import Foundation

@Derive(Equatable, Clone, Hashable, Serializable, Ordered)
distinct DsdTabId = U32

@Main
function main() {
    let first = DsdTabId(U32(41))
    let copied = clone_DsdTabId(first)
    print(eq_DsdTabId(first, copied))
    print(eq_DsdTabId(first, DsdTabId(U32(42))))
    print(hash_DsdTabId(first) == hash_DsdTabId(copied))
    print(hash_DsdTabId(first) == hash_DsdTabId(DsdTabId(U32(42))))
    let wire = serialize_DsdTabId(first)
    print(wire)
    print(eq_DsdTabId(deserialize_DsdTabId(wire), first))
    print(lt_DsdTabId(first, DsdTabId(U32(42))))
    print(lt_DsdTabId(DsdTabId(U32(42)), first))
    return
}
"#,
    );
    assert_eq!(output, "true\nfalse\ntrue\nfalse\nDsdTabId(U32(41))\ntrue\ntrue\nfalse\n");
}

/// A distinct writes its name around its representation: `DswPort(U32(8080))`.
/// The outer tag keeps the nominal identity the language keeps apart from the
/// representation, and the inner value is written exactly as it would be
/// anywhere else, so one reader shape serves every position.
#[test]
fn serialization_writes_the_representation_and_nothing_around_it() {
    let output = assert_parity(
        r#"
import Foundation

@Derive(Serializable, Equatable)
distinct DswPort = U32

@Derive(Serializable)
struct DswEndpoint {
    var port: DswPort
}

@Main
function main() {
    print(serialize_DswPort(DswPort(U32(8080))))
    print(serialize_DswEndpoint(DswEndpoint { port: DswPort(U32(8080)) }))
    print(eq_DswPort(deserialize_DswPort("DswPort(U32(8080))"), DswPort(U32(8080))))
    return
}
"#,
    );
    assert_eq!(output, "DswPort(U32(8080))\nDswEndpoint{port:DswPort(U32(8080))}\ntrue\n");
}

/// `Option<Value>` over a distinct type: an ordinary generic enum, matched the
/// ordinary way, on both backends.
#[test]
fn option_over_a_distinct_type_agrees() {
    let output = assert_parity(
        r#"
import Foundation

distinct DsoTabId = U32

function dsoOpen(selected: borrow Option<DsoTabId>) -> Int {
    match selected {
        Some(id) -> return Int(id.raw)
        None -> return -1
    }
}

@Main
function main() {
    let found: Option<DsoTabId> = .Some(DsoTabId(U32(3)))
    let missing: Option<DsoTabId> = .None
    print(dsoOpen(found))
    print(dsoOpen(missing))
    return
}
"#,
    );
    assert_eq!(output, "3\n-1\n");
}
