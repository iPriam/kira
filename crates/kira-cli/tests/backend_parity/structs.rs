//! Parity for struct construction, copying, fields, and methods.

use crate::{assert_module_parity, assert_parity, run_on, write_source};

#[test]
fn struct_fields_and_defaults_agree() {
    let output = assert_parity(
        r#"
struct Pair {
    var w: Int = 1
    var h: Int = 2
}

@Main
function main() {
    let given = Pair { w = 10, h = 20 }
    print(given.w + given.h)
    // Every field defaulted.
    let blank = Pair {}
    print(blank.w + blank.h)
    // One field defaulted, and the `:` binder still parses.
    let partial = Pair { h: 5 }
    print(partial.w + partial.h)
    return
}
"#,
    );
    assert_eq!(output, "30\n3\n6\n");
}

/// An omitted field is the value its declaring module resolved, even when the
/// construction site has none of that module's helper imports in scope.
#[test]
fn a_cross_module_field_default_agrees() {
    let output = assert_module_parity(
        "import definitions\n\
         @Main function main() {\n\
             let holder = CallbackHolder {}\n\
             print(holder.seed)\n\
             print(holder.callback())\n\
             return\n\
         }",
        &[
            ("helper", "function helperValue() -> Int { return 41 }"),
            (
                "definitions",
                "import helper as H\n\
                 function moduleDefault() -> Int { return H.helperValue() + 1 }\n\
                 struct CallbackHolder {\n\
                     let seed: Int = H.helperValue()\n\
                     let callback: () -> Int = moduleDefault\n\
                 }",
            ),
        ],
    );
    assert_eq!(output.as_bytes(), b"41\n42\n");
}

/// A struct field may name a struct declared later in the file: struct
/// collection is two-phase, so `Outer` (the lower id) holding `Inner` (declared
/// after it) compiles on every backend. This exercises the native backend's
/// two-pass struct declaration — a by-value field of a struct at a higher id —
/// which a single pass in declaration order would leave undefined.
#[test]
fn a_struct_field_naming_a_later_struct_agrees() {
    let output = assert_parity(
        r#"
struct Outer {
    var inner: Inner
    var tag: Int = 7
}

struct Inner {
    var depth: Int = 3
}

@Main
function main() {
    let o = Outer { inner: Inner { depth: 5 } }
    print(o.inner.depth + o.tag)
    // `Inner {}` takes its field default; `Outer`'s `tag` takes its own.
    let d = Outer { inner: Inner {} }
    print(d.inner.depth + d.tag)
    return
}
"#,
    );
    assert_eq!(output, "12\n10\n");
}

#[test]
fn copying_a_struct_does_not_alias_it() {
    // The rule the reference corpus pins: `var a2 = a1; a2.x = 999` must leave
    // `a1` alone. A shallow copy on either backend fails exactly here.
    let output = assert_parity(
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
    assert_eq!(output, "5\n999\n");
}

#[test]
fn a_string_field_is_copied_not_shared() {
    // The case where a shallow copy is not just wrong but unsound: two structs
    // sharing one string handle would double-free it at scope exit. The VM
    // proves its heap balances; the native backend has no such proof, so this
    // is what stands in for one.
    let output = assert_parity(
        r#"
struct Labelled {
    var label: String
    var count: Int
}

@Main
function main() {
    var first = Labelled { label = "original", count = 1 }
    var second = first
    second.label = "replaced"
    print(first.label)
    print(second.label)
    first.label = first.label + "!"
    print(first.label)
    print(second.label)
    return
}
"#,
    );
    assert_eq!(output, "original\nreplaced\noriginal!\nreplaced\n");
}

#[test]
fn nested_struct_writes_land_in_place() {
    let output = assert_parity(
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
    o.tag = 7
    print(o.tag)
    // A copy taken after the write is independent of later writes.
    var copy = o
    o.middle.inner.value = 100
    print(copy.middle.inner.value)
    print(o.middle.inner.value)
    return
}
"#,
    );
    assert_eq!(output, "42\n7\n42\n100\n");
}

#[test]
fn structs_cross_function_boundaries_by_value() {
    let output = assert_parity(
        r#"
struct Vec3 {
    var x: Int
    var y: Int
    var z: Int
}

function sum(v: borrow Vec3) -> Int {
    return v.x + v.y + v.z
}

function scaled(v: borrow Vec3, k: Int) -> Vec3 {
    return Vec3 { x = v.x * k, y = v.y * k, z = v.z * k }
}

function bump(v: borrow Vec3) -> Int {
    // Mutating a parameter must not reach the caller's value.
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
    assert_eq!(output, "6\n60\n1000\n6\n");
}

#[test]
fn a_struct_carrying_a_string_survives_a_loop() {
    // A loop is where a leak or a double free shows up rather than hides: the
    // VM's heap accounting would catch an imbalance, and the native backend
    // would crash on a second free.
    let output = assert_parity(
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
    assert_eq!(output, "xy\n2\n");
}

#[test]
fn a_struct_at_the_native_seam_is_refused_with_a_reason() {
    // Structs work on both engines; only the crossing between them is unbuilt.
    // A build that would need one to cross must say so, in terms a user can
    // act on, rather than emit a crossing that marshals the wrong shape.
    let path = write_source(
        r#"
struct Point {
    var x: Int
}

@Native
function takes(p: Point) -> Int {
    return p.x
}

@Main
function main() {
    print(takes(Point { x = 1 }))
    return
}
"#,
    );
    let run = run_on(&path, "hybrid");
    assert_eq!(
        run.status.code(),
        Some(1),
        "a struct crossing the seam must fail the build",
    );
    let stderr = String::from_utf8_lossy(&run.stderr);
    assert!(
        stderr.contains("cannot cross"),
        "the failure must name what went wrong, got: {stderr}",
    );
    let _ = std::fs::remove_dir_all(path.parent().expect("program directory"));
}

#[test]
fn a_struct_inside_one_engine_still_builds_a_hybrid_program() {
    // The other half of the rule above: a struct in a signature that never
    // crosses is an ordinary program. The manifest describes it; nothing
    // marshals it.
    let output = assert_parity(
        r#"
struct Point {
    var x: Int
    var y: Int
}

function local_only(p: borrow Point) -> Int {
    return p.x + p.y
}

@Native
function double(value: Int) -> Int {
    return value * 2
}

@Main
function main() {
    let p = Point { x = 3, y = 4 }
    print(double(local_only(p)))
    return
}
"#,
    );
    assert_eq!(output, "14\n");
}

#[test]
fn struct_methods_agree() {
    // A method is an ordinary call with the receiver as argument 0 — that is
    // the whole implementation, and it is why no backend needed changing. This
    // is what says the claim is true rather than plausible.
    let output = assert_parity(
        r#"
struct Point {
    var x: Int
    var y: Int

    function sum() -> Int { return self.x + self.y }
    function scale(k: Int) -> Point { return Point { x = self.x * k, y = self.y * k } }
    function larger() -> Int { if self.x > self.y { return self.x } return self.y }
}

struct Counter {
    let step: Int = 1

    // A method may name a field bare, without `self.`.
    function next(value: Int) -> Int { return value + step }
}

@Main
function main() {
    let p = Point { x = 3, y = 4 }
    print(p.sum())
    // Chained: the result of a method is a value like any other.
    print(p.scale(10).sum())
    print(p.larger())
    let c = Counter {}
    print(c.next(41))
    return
}
"#,
    );
    assert_eq!(output, "7\n70\n4\n42\n");
}

#[test]
fn a_method_receives_its_receiver_by_value() {
    // The receiver is a copy, so writing to it inside the method is invisible
    // outside — the same rule an ordinary by-value parameter follows.
    let output = assert_parity(
        r#"
struct Tally {
    var n: Int
    var name: String

    function bumped() -> Int {
        var local = self
        local.n = 999
        local.name = "changed"
        return local.n
    }
}

@Main
function main() {
    let t = Tally { n = 1, name = "kept" }
    print(t.bumped())
    print(t.n)
    print(t.name)
    return
}
"#,
    );
    assert_eq!(output, "999\n1\nkept\n");
}
