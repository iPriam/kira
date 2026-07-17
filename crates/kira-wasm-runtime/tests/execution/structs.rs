//! Parity for struct construction, copying, fields, and methods.

use crate::assert_parity;

#[test]
fn struct_fields_and_defaults_agree() {
    assert_parity(
        r#"
struct Pair {
    var w: Int = 1
    var h: Int = 2
}

@Main
function main() {
    let given = Pair { w = 10, h = 20 }
    print(given.w + given.h)
    let blank = Pair {}
    print(blank.w + blank.h)
    return
}
"#,
    );
}

#[test]
fn copying_a_struct_does_not_alias_it() {
    assert_parity(
        r#"
struct Vec3 {
    var x: Int
    var y: Int
    var z: Int
}

@Main
function main() {
    let a1 = Vec3 { x = 5, y = 5, z = 5 }
    var a2 = a1
    a2.x = 999
    print(a1.x)
    print(a2.x)
    return
}
"#,
    );
}

#[test]
fn nested_struct_writes_land_in_place() {
    assert_parity(
        r#"
struct Inner {
    var value: Int
}

struct Middle {
    var inner: Inner
}

struct Outer {
    var middle: Middle
    var tag: Int = 0
}

@Main
function main() {
    var o = Outer { middle = Middle { inner = Inner { value = 1 } } }
    o.middle.inner.value = 42
    print(o.middle.inner.value)
    var copy = o
    o.middle.inner.value = 100
    print(copy.middle.inner.value)
    print(o.middle.inner.value)
    return
}
"#,
    );
}

#[test]
fn structs_of_mixed_field_types_agree() {
    // Every field width in one object, so a layout mistake shows up as a wrong
    // value rather than hiding behind uniform slots — and the offsets differ
    // between wasm32 and wasm64, which both devices check.
    assert_parity(
        r#"
struct Mixed {
    var flag: Bool
    var count: Int
    var ratio: Float
    var name: String
}

@Main
function main() {
    var m = Mixed { flag = true, count = 7, ratio = 0.5, name = "kira" }
    print(m.flag)
    print(m.count)
    print(m.ratio)
    print(m.name)
    m.count = -9
    m.flag = false
    print(m.count)
    print(m.flag)
    print(m.name)
    return
}
"#,
    );
}

#[test]
fn structs_cross_function_boundaries_by_value() {
    assert_parity(
        r#"
struct Vec3 {
    var x: Int
    var y: Int
    var z: Int
}

function sum(v: Vec3) -> Int {
    return v.x + v.y + v.z
}

function scaled(v: Vec3, k: Int) -> Vec3 {
    return Vec3 { x = v.x * k, y = v.y * k, z = v.z * k }
}

function bump(v: Vec3) -> Int {
    var local = v
    local.x = 1000
    return local.x
}

@Main
function main() {
    let v = Vec3 { x = 1, y = 2, z = 3 }
    print(sum(v))
    print(sum(scaled(v, 10)))
    print(bump(v))
    print(sum(v))
    return
}
"#,
    );
}

#[test]
fn a_struct_built_in_a_loop_agrees() {
    assert_parity(
        r#"
struct Item {
    var name: String
    var n: Int
}

@Main
function main() {
    var i = 0
    var acc = Item { name = "", n = 0 }
    while i < 3 {
        var next = Item { name = "x", n = i }
        next.name = next.name + "y"
        acc = next
        i = i + 1
    }
    print(acc.name)
    print(acc.n)
    return
}
"#,
    );
}

#[test]
fn struct_methods_agree() {
    assert_parity(
        r#"
struct Point {
    var x: Int
    var y: Int

    function sum() -> Int { return self.x + self.y }
    function scale(k: Int) -> Point { return Point { x = self.x * k, y = self.y * k } }
}

struct Counter {
    let step: Int = 1

    function next(value: Int) -> Int { return value + step }
}

@Main
function main() {
    let p = Point { x = 3, y = 4 }
    print(p.sum())
    print(p.scale(10).sum())
    let c = Counter {}
    print(c.next(41))
    return
}
"#,
    );
}
