//! Parity for `Any`, the top type: a value of every kind crossing into it, and
//! being carried, copied, and dropped once it has.
//!
//! The two engines do genuinely different things here, which is why these cases
//! matter more than most. The VM's `Value` is a tagged union, so erasure is the
//! identity and its bytecode compiler emits no instruction at all; native code
//! has nowhere to keep the tag, so the LLVM backend allocates a box for every
//! crossing. A case that diverges means one of those two is wrong about what an
//! erased value owns.
//!
//! Nothing here reads an `Any` back, because nothing can: the language has no
//! `is`, `as`, or downcast form. So what is proven is the half that exists —
//! that a value crosses in, and survives being stored, copied, passed,
//! returned, and released on all three engines with the same output and the
//! same exit status.
//!
//! # What these do not prove
//!
//! This harness compares stdout and exit status; it runs no heap accounting, so
//! an erased value whose box forgot it owned bytes leaks here silently rather
//! than failing. What the cases do catch is the louder half of that bug class —
//! a box freed as the wrong kind, or a payload read as a pointer that was a
//! scalar, traps or diverges — and
//! [`super::any::erasing_in_a_loop_stays_consistent`] is sized so a per-crossing leak is
//! a thousand allocations rather than one. A real balance assertion needs
//! per-program accounting on the native side, which this workspace does not yet
//! expose; the claim is stated here rather than assumed.

use crate::assert_parity;

/// A scalar of each kind crosses in.
///
/// The three scalars own nothing, so this is the case where a box that
/// mistakenly freed its payload would corrupt rather than leak.
#[test]
fn scalars_cross_into_any() {
    let output = assert_parity(
        r#"
@Main
function main() {
    let i: Any = 7
    let f: Any = 2.5
    let b: Any = true
    print("scalars")
    return
}
"#,
    );
    assert_eq!(output, "scalars\n");
}

/// A `String` crosses in, and the box takes over freeing its bytes.
#[test]
fn a_string_crosses_into_any() {
    let output = assert_parity(
        r#"
@Main
function main() {
    let s: Any = "erased"
    let t: Any = "erased too"
    print("string")
    return
}
"#,
    );
    assert_eq!(output, "string\n");
}

/// A struct crosses in whole, including a field that owns heap storage.
///
/// The interesting half is the `String` field: the erased aggregate carries the
/// clone and free leaves for the struct, so a copy of the box duplicates the
/// field rather than sharing or dropping it.
#[test]
fn a_struct_crosses_into_any() {
    let output = assert_parity(
        r#"
struct Pair {
    let count: Int
    let label: String
}

@Main
function main() {
    let p: Any = Pair(count: 3, label: "inner")
    print("struct")
    return
}
"#,
    );
    assert_eq!(output, "struct\n");
}

/// An array crosses in, and its elements go with it.
#[test]
fn an_array_crosses_into_any() {
    let output = assert_parity(
        r#"
@Main
function main() {
    let numbers: [Int] = [1, 2, 3]
    let erased: Any = move numbers
    let words: [String] = ["a", "b"]
    let alsoErased: Any = move words
    print("array")
    return
}
"#,
    );
    assert_eq!(output, "array\n");
}

/// An enum crosses in, payload and all.
#[test]
fn an_enum_crosses_into_any() {
    let output = assert_parity(
        r#"
enum Shade {
    Dim
    Bright(String)
}

@Main
function main() {
    let plain: Shade = .Dim
    let erasedPlain: Any = move plain
    let loud: Shade = .Bright("noisy")
    let erasedLoud: Any = move loud
    print("enum")
    return
}
"#,
    );
    assert_eq!(output, "enum\n");
}

/// `Any` in a parameter and a result: the crossing happens at the call site,
/// and the callee only ever sees the erased form.
#[test]
fn any_passes_through_a_call() {
    let output = assert_parity(
        r#"
function keep(value: Any) -> Any {
    return value
}

@Main
function main() {
    let once: Any = keep(1)
    let twice: Any = keep("text")
    let thrice: Any = keep(move once)
    print("call")
    return
}
"#,
    );
    assert_eq!(output, "call\n");
}

/// An array *of* `Any`: each element erases as it goes in, and `.count` still
/// reads the array rather than anything about what it holds.
#[test]
fn an_array_of_any_holds_mixed_kinds() {
    let output = assert_parity(
        r#"
struct Pair {
    let count: Int
    let label: String
}

@Main
function main() {
    let mixed: [Any] = [1, 2.5, true, "text", Pair(count: 1, label: "in")]
    print(mixed.count)
    return
}
"#,
    );
    assert_eq!(output, "5\n");
}

/// An `Any` field of a struct, which is what makes the element leaves recurse:
/// copying the struct has to copy the erased value, and dropping it has to
/// release it.
#[test]
fn a_struct_field_of_any_copies_and_drops() {
    let output = assert_parity(
        r#"
struct Slot {
    let name: String
    let payload: Any
}

@Main
function main() {
    let one = Slot(name: "first", payload: "erased string")
    let two = Slot(name: "second", payload: 42)
    let copied = one
    print(copied.name)
    print(two.name)
    return
}
"#,
    );
    assert_eq!(output, "first\nsecond\n");
}

/// An `Any` enum payload: the erased value is boxed inside another box, so the
/// nested release has to reach it.
#[test]
fn an_enum_payload_of_any_releases_its_box() {
    let output = assert_parity(
        r#"
enum Held {
    Nothing
    Something(Any)
}

@Main
function main() {
    let empty: Held = .Nothing
    let full: Held = .Something("erased")
    let number: Held = .Something(11)
    print("payload")
    return
}
"#,
    );
    assert_eq!(output, "payload\n");
}

/// Reassignment through an `Any` binding: the previous erased value is released
/// before the new one lands, which is where a double free or a leak would show.
#[test]
fn reassigning_an_any_releases_what_it_held() {
    let output = assert_parity(
        r#"
@Main
function main() {
    var slot: Any = "first"
    slot = "second"
    slot = 3
    slot = 4.5
    print("reassigned")
    return
}
"#,
    );
    assert_eq!(output, "reassigned\n");
}

/// `Any` through a class constructor, a construct's inputs, a family method's
/// dispatcher, and a closure.
///
/// Each of these checks its arguments in its own code path, so each is its own
/// chance to accept a value into an `Any` slot without erasing it — which the VM
/// would carry on regardless and native code would read as a pointer that was a
/// scalar.
#[test]
fn any_crosses_at_every_kind_of_call_site() {
    let output = assert_parity(
        r#"
class Holder {
    let held: Any
}

construct Measure {
    @Required function reading() -> Any
}

Measure Depth(seed: Any) {
    reading { 12 }
}

function callWith(f: (Any) -> Int, value: Any) -> Int {
    return f(move value)
}

function count(erased: Any) -> Int {
    return 1
}

@Main
function main() {
    let boxed = Holder(3)
    let built = Depth(seed: "erased input")
    let read: Any = built.reading
    let viaClosure = callWith(count, 4.5)
    print(viaClosure)
    return
}
"#,
    );
    assert_eq!(output, "1\n");
}

/// A loop that erases repeatedly.
///
/// Sized so that a per-crossing mistake in the box has a thousand chances to
/// trap or diverge rather than one. It does not assert a balance — see the
/// module header for why nothing here can yet.
#[test]
fn erasing_in_a_loop_stays_consistent() {
    let output = assert_parity(
        r#"
@Main
function main() {
    var seen = 0
    for i in 0..1000 {
        let erased: Any = "allocated each time"
        let also: Any = i
        seen = seen + 1
    }
    print(seen)
    return
}
"#,
    );
    assert_eq!(output, "1000\n");
}

/// Two erased values compare by structure once their types agree.
///
/// The half of `Any` that used to be missing. Nothing can read an erased value
/// back — there is still no `is`, `as`, or downcast — but two of them can be
/// asked whether they are the same value, which is what a test runner written
/// in Kira needs to compare a case's result against its expectation.
#[test]
fn erased_values_compare_by_structure() {
    let output = assert_parity(
        r#"
struct Point { var x: Int = 0
    var y: Int = 0 }

enum Shade { Light
    Mid }

@Main
function main() {
    let seven: Any = 7
    let alsoSeven: Any = 7
    let eight: Any = 8
    print(seven == alsoSeven)
    print(seven == eight)
    print(seven != eight)

    // Built two different ways, so an equal answer is about the bytes rather
    // than about the two sides being one object.
    let greeting: Any = "hi"
    let assembled: Any = "h" + "i"
    print(greeting == assembled)

    let here: Any = Point(x: 1, y: 2)
    let alsoHere: Any = Point(x: 1, y: 2)
    let elsewhere: Any = Point(x: 1, y: 3)
    print(here == alsoHere)
    print(here == elsewhere)

    let counts: Any = [1, 2, 3]
    let sameCounts: Any = [1, 2, 3]
    let fewer: Any = [1, 2]
    print(counts == sameCounts)
    print(counts == fewer)

    let light: Any = Shade.Light
    let alsoLight: Any = Shade.Light
    let mid: Any = Shade.Mid
    print(light == alsoLight)
    print(light == mid)
    return
}
"#,
    );
    assert_eq!(
        output,
        "true\nfalse\ntrue\ntrue\ntrue\nfalse\ntrue\nfalse\ntrue\nfalse\n"
    );
}

/// Values of different types are unequal, even with identical structure.
///
/// The case that decided erasure carries a type id rather than a coarse kind.
/// Without one the VM would say `true` here — its struct objects are tuples
/// with no record of which declaration built them — and the LLVM backend could
/// not answer at all, because an erased aggregate is untyped bytes and reading
/// a `Rect`'s through a `Point`'s generated leaf is undefined behavior rather
/// than a wrong answer.
#[test]
fn two_types_with_one_shape_are_not_equal_erased() {
    let output = assert_parity(
        r#"
struct Point { var x: Int = 0
    var y: Int = 0 }
struct Rect { var x: Int = 0
    var y: Int = 0 }

enum Left { One
    Two }
enum Right { One
    Two }

@Main
function main() {
    let point: Any = Point(x: 1, y: 2)
    let rect: Any = Rect(x: 1, y: 2)
    print(point == rect)

    let left: Any = Left.One
    let right: Any = Right.One
    print(left == right)

    // A different kind entirely is unequal rather than a trap: `Any` is the
    // one type whose operands are not known to agree statically.
    let number: Any = 1
    let text: Any = "1"
    print(number == text)
    return
}
"#,
    );
    assert_eq!(output, "false\nfalse\nfalse\n");
}

/// A widened payload compares equal to a directly erased one.
///
/// `Result<Int, E>` -> `Result<Any, E>` is the path a test runner's `expect`
/// takes, and it is the one erasure boxing broke: the widened payload has to
/// become the same erasure box a direct crossing produces, or the two compare
/// unequal while holding the same value. The VM rebuilds through a synthesized
/// helper and the LLVM backend through a generated leaf, which is exactly the
/// kind of difference this suite exists to hold to one behavior.
#[test]
fn a_widened_payload_equals_a_directly_erased_one() {
    let output = assert_parity(
        r#"
enum AppError { NotFound }
enum Result<Value, Failure> { Ok(Value) Error(Failure) }

struct Point { var x: Int = 0
    var y: Int = 0 }

function wrapInt() -> Result<Any, AppError> {
    let narrow: Result<Int, AppError> = .Ok(10)
    return narrow
}

function wrapPoint() -> Result<Any, AppError> {
    let narrow: Result<Point, AppError> = .Ok(Point(x: 1, y: 2))
    return narrow
}

@Main
function main() {
    let direct: Any = 10
    match wrapInt() {
        Ok(v) -> { print(direct == v) }
        Error(e) -> { print("error") }
    }
    let other: Any = 11
    match wrapInt() {
        Ok(v) -> { print(other == v) }
        Error(e) -> { print("error") }
    }
    let here: Any = Point(x: 1, y: 2)
    match wrapPoint() {
        Ok(v) -> { print(here == v) }
        Error(e) -> { print("error") }
    }
    return
}
"#,
    );
    assert_eq!(output, "true\nfalse\ntrue\n");
}
