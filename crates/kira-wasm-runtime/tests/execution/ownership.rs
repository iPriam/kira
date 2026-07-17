//! Parity for the ownership modes on wasm.
//!
//! Ownership is checked in the analyzer and reaches no backend, so these cases
//! ask the narrow question wasm can answer: does a program written with `move`,
//! `borrow`, and `copy` compute the same thing here as on the VM?
//!
//! It is worth asking separately from the native parity suite because the wasm
//! heap is a bump allocator that never frees. A backend that had quietly
//! treated `move` as an alias would go unnoticed there — nothing is reclaimed,
//! so nothing can be freed twice — and would still have to produce the same
//! arithmetic to pass here.

use crate::assert_parity;

/// Moving a struct into a consuming callee, repeatedly, in a loop.
#[test]
fn moving_a_struct_agrees() {
    assert_parity(
        r#"
struct Vec3 {
    var x: Int
    var y: Int
    var z: Int
}

function consume(v: Vec3) -> Int {
    return v.x + v.y + v.z
}

@Main
function main() {
    var acc = 0
    var i = 0
    while i < 10 {
        let v = Vec3 { x: i, y: i, z: i }
        acc = acc + consume(move v)
        i = i + 1
    }
    print(acc)
    return
}
"#,
    );
}

/// A borrow leaves the caller's value readable afterwards.
#[test]
fn borrowing_a_struct_agrees() {
    assert_parity(
        r#"
struct Vec3 {
    var x: Int
    var y: Int
    var z: Int
}

function vecSum(v: borrow Vec3) -> Int {
    return v.x + v.y + v.z
}

@Main
function main() {
    let v = Vec3 { x: 5, y: 6, z: 7 }
    print(vecSum(v))
    print(vecSum(v))
    print(v.x)
    return
}
"#,
    );
}

/// Binding a struct copies it, so writing the copy leaves the source alone.
#[test]
fn binding_a_struct_copies_it() {
    assert_parity(
        r#"
struct Vec3 {
    var x: Int
    var y: Int
    var z: Int
}

@Main
function main() {
    let v = Vec3 { x: 5, y: 6, z: 7 }
    var w = v
    w.x = 100
    w.y = 0
    print(v.x)
    print(v.y)
    print(w.x)
    return
}
"#,
    );
}

/// Strings own heap bytes, so a move of one exercises the allocator.
#[test]
fn moving_a_string_agrees() {
    assert_parity(
        r#"
function shout(s: String) -> String {
    return s + "!"
}

function isKira(s: borrow String) -> Bool {
    return s == "kira"
}

@Main
function main() {
    let name = "kira"
    print(isKira(name))
    print(shout(move name))
    return
}
"#,
    );
}

/// `copy` on a scalar is accepted and is a no-op.
#[test]
fn copying_a_scalar_agrees() {
    assert_parity(
        r#"
function twice(n: copy Int) -> Int {
    return n + n
}

@Main
function main() {
    let n = 21
    print(twice(copy n))
    print(n)
    return
}
"#,
    );
}
