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
use crate::{assert_parity, assert_parity_with_heap_balance, assert_trap_parity};

/// A scalar of each kind crosses in.
///
/// The three scalars own nothing, so this is the case where a box that
/// mistakenly freed its payload would corrupt rather than leak.
#[test]
fn scalars_cross_into_any() {
    let output = assert_parity_with_heap_balance(
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

/// A typed foreign pointer crosses as the same inert word as `RawPtr`.
#[test]
fn a_foreign_pointer_crosses_into_any() {
    let output = assert_parity_with_heap_balance(
        r#"
@FFI.Struct { layout: c }
struct Byte {
    let value: U8
}

@FFI.Pointer { target: Byte, ownership: borrowed }
struct BytePtr {}

@Main
function main() {
    let raw: RawPtr = RawPtr(0)
    let typed: BytePtr = RawPtr(0)
    let first: Any = raw
    let second: Any = typed
    print(first == second)
    return
}
"#,
    );
    assert_eq!(output, "true\n");
}

/// A `String` crosses in, and the box takes over freeing its bytes.
#[test]
fn a_string_crosses_into_any() {
    let output = assert_parity_with_heap_balance(
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
    let output = assert_parity_with_heap_balance(
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
    let output = assert_parity_with_heap_balance(
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
    let output = assert_parity_with_heap_balance(
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
    let output = assert_parity_with_heap_balance(
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
    let output = assert_parity_with_heap_balance(
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
    let output = assert_parity_with_heap_balance(
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

/// Aggregate values inside `Any` fields survive a struct copy and compare by
/// contents before every nested box is released.
#[test]
fn aggregate_any_fields_are_copied_compared_and_dropped() {
    let output = assert_parity_with_heap_balance(
        r#"
struct Pair {
    let count: Int
    let label: String
}

struct Slot {
    let payload: Any
}
@Main
function main() {
    let pair = Slot(payload: Pair(count: 3, label: "pair"))
    let pairCopy = pair
    let rows = Slot(payload: [[1, 2], [3]])
    let rowsCopy = rows
    print(pair.payload == pairCopy.payload)
    print(rows.payload == rowsCopy.payload)
    return
}
"#,
    );
    assert_eq!(output, "true\ntrue\n");
}

/// An `Any` enum payload: the erased value is boxed inside another box, so the
/// nested release has to reach it.
#[test]
fn an_enum_payload_of_any_releases_its_box() {
    let output = assert_parity_with_heap_balance(
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
    let output = assert_parity_with_heap_balance(
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
    let output = assert_parity_with_heap_balance(
        r#"
class Holder {
    let held: Any
}

construct Measure {
    @Required function reading() -> Any
}

construct Depth(seed: Any) extends Measure {
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
/// trap or diverge rather than one, while the native report checks every
/// allocation is released.
#[test]
fn erasing_in_a_loop_stays_consistent() {
    let output = assert_parity_with_heap_balance(
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
/// Erased values still have no `is`, `as`, or downcast operation, but equality
/// can compare two values with the same erased shape.
#[test]
fn erased_values_compare_by_structure() {
    let output = assert_parity_with_heap_balance(
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
    let output = assert_parity_with_heap_balance(
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

/// A payload erased by a rebuild compares equal to a directly erased one.
///
/// `Result<Int, E>` -> `Result<Any, E>` is the path a test runner's `expect`
/// takes, and it is written rather than implied: the program unpacks one
/// specialization and builds the other, erasing the payload where it writes
/// it. The rebuilt payload has to be the same erasure box a direct crossing
/// produces, or the two compare unequal while holding the same value.
#[test]
fn a_rebuilt_payload_equals_a_directly_erased_one() {
    let output = assert_parity_with_heap_balance(
        r#"
enum AppError { NotFound }
enum Result<Value, Failure> { Ok(Value) Error(Failure) }

struct Point { var x: Int = 0
    var y: Int = 0 }

function wrapInt() -> Result<Any, AppError> {
    let narrow: Result<Int, AppError> = .Ok(10)
    match narrow {
        Ok(value) -> { return .Ok(value) }
        Error(failure) -> { return .Error(failure) }
    }
}

function wrapPoint() -> Result<Any, AppError> {
    let narrow: Result<Point, AppError> = .Ok(Point(x: 1, y: 2))
    match narrow {
        Ok(value) -> { return .Ok(value) }
        Error(failure) -> { return .Error(failure) }
    }
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

/// An erased enum keeps a struct payload's nominal identity and owned fields.
#[test]
fn an_erased_enum_with_a_struct_payload_round_trips_and_projects() {
    let output = assert_parity_with_heap_balance(
        r#"
struct Record {
    let code: Int
    let name: String
}

struct EnvelopePayload {
    let record: Record
    let rows: [[Int]]
}

enum Envelope {
    Record(Record)
    Nested(EnvelopePayload)
    Empty
}

enum Carrier<Value> {
    Some(Value)
    None
}

function erase(value: Envelope) -> Any {
    return move value
}

function rebuilt() -> Carrier<Any> {
    let narrow: Carrier<Record> = .Some(Record { code: 7, name: "kept" })
    match narrow {
        Some(value) -> { return .Some(value) }
        None -> { return .None }
    }
}

function project(value: Carrier<Any>) -> Bool {
    let expected: Any = Record { code: 7, name: "kept" }
    match value {
        Some(item) -> return item == expected
        None -> return false
    }
}

function projectEnvelope(value: Envelope) -> Bool {
    match value {
        Record(item) -> return item.code == 7 && item.name == "kept"
        Nested(item) -> return item.record.code == 7 && item.rows.count == 2
        Empty -> return false
    }
}

function keep(value: Any) -> Any {
    return value
}

@Main
function main() {
    let first: Any = Envelope.Record(Record { code: 7, name: "kept" })
    let same: Any = Envelope.Record(Record { code: 7, name: "kept" })
    let changed: Any = Envelope.Record(Record { code: 8, name: "kept" })
    let nested: Any = Envelope.Nested(EnvelopePayload {
        record: Record { code: 7, name: "kept" },
        rows: [[1, 2], [3]]
    })
    let nestedSame: Any = Envelope.Nested(EnvelopePayload {
        record: Record { code: 7, name: "kept" },
        rows: [[1, 2], [3]]
    })
    let erased: Any = Envelope.Record(Record { code: 7, name: "kept" })
    print(first == same)
    print(first == changed)
    print(keep(move first) == same)
    print(nested == nestedSame)
    print(erase(Envelope.Record(Record { code: 7, name: "kept" })) == erased)
    print(project(rebuilt()))
    print(projectEnvelope(Envelope.Record(Record { code: 7, name: "kept" })))
    return
}
"#,
    );
    assert_eq!(output, "true\nfalse\ntrue\ntrue\ntrue\ntrue\ntrue\n");
}

/// Nested arrays and structs retain their aggregate leaves through erasure.
#[test]
fn an_erased_enum_with_nested_array_payload_balances_and_projects() {
    let output = assert_parity_with_heap_balance(
        r#"
struct Point {
    let x: Int
    let label: String
}

enum Item {
    Point(Point)
    Empty
}

enum Batch {
    Points([Point])
    Rows([[Int]])
    Items([Item])
    Empty
}

enum Carrier<Value> {
    Some(Value)
    None
}

function erase(value: Batch) -> Any {
    return move value
}

function rebuilt() -> Carrier<Any> {
    let narrow: Carrier<[[Point]]> = .Some([
        [Point { x: 1, label: "a" }],
        [Point { x: 2, label: "b" }]
    ])
    match narrow {
        Some(value) -> { return .Some(value) }
        None -> { return .None }
    }
}

function project(value: Carrier<Any>) -> Bool {
    let expected: Any = [
        [Point { x: 1, label: "a" }],
        [Point { x: 2, label: "b" }]
    ]
    match value {
        Some(item) -> {
            return item == expected
        }
        None -> return false
    }
}

function projectBatch(value: Batch) -> Bool {
    match value {
        Points(items) -> return items.count == 2 && items[0].x == 1 && items[1].x == 2
        Rows(rows) -> return rows.count == 2 && rows[0].count == 2 && rows[1].count == 1
        Items(items) -> return items.count == 2
        Empty -> return false
    }
}

@Main
function main() {
    let first: Any = Batch.Points([
        Point { x: 1, label: "a" },
        Point { x: 2, label: "b" }
    ])
    let same: Any = Batch.Points([
        Point { x: 1, label: "a" },
        Point { x: 2, label: "b" }
    ])
    let changed: Any = Batch.Points([
        Point { x: 1, label: "a" },
        Point { x: 3, label: "b" }
    ])
    let nested: Any = Batch.Items([
        Item.Point(Point { x: 1, label: "a" }),
        Item.Empty
    ])
    let nestedSame: Any = Batch.Items([
        Item.Point(Point { x: 1, label: "a" }),
        Item.Empty
    ])
    let rows: Any = Batch.Rows([[1, 2], [3]])
    print(first == same)
    print(first == changed)
    print(nested == nestedSame)
    print(erase(Batch.Rows([[1, 2], [3]])) == rows)
    print(project(rebuilt()))
    print(projectBatch(Batch.Points([
        Point { x: 1, label: "a" },
        Point { x: 2, label: "b" }
    ])))
    return
}
"#,
    );
    assert_eq!(output, "true\nfalse\ntrue\ntrue\ntrue\ntrue\n");
}

/// `is` answers by runtime identity and `as` hands the held value back, on
/// every backend.
#[test]
fn is_and_as_read_an_erased_value_back() {
    let output = assert_parity(
        r#"
struct Point {
    let x: Int
}

@Main
function main() {
    let boxed: Any = Point(x: 41)
    let number: Any = 8
    print(boxed is Point)
    print(boxed is Int)
    print(number is Int)
    print((boxed as Point).x + (number as Int))
    return
}
"#,
    );
    assert_eq!(output, "true\nfalse\ntrue\n49\n");
}

/// A cast to a type the `Any` does not hold traps, after the output before it.
#[test]
fn a_cast_to_the_wrong_type_traps() {
    assert_trap_parity(
        r#"
struct Point {
    let x: Int
}

@Main
function main() {
    let boxed: Any = 8
    print(boxed is Point)
    let point = boxed as Point
    print(point.x)
    return
}
"#,
        "false\n",
    );
}

/// `value.type` answers the same identity, name, kind, arguments, and
/// conformances on every backend.
///
/// The two engines answer from different places — the VM reads the descriptor
/// table its module carries, native code runs a generated reader over the same
/// rows — so this is exactly the kind of split that has to produce one answer.
#[test]
fn a_runtime_type_descriptor_agrees_on_every_backend() {
    let output = assert_parity_with_heap_balance(
        r#"
trait Greets {
    function greet(borrow self) -> String
}

struct Point {
    let x: Int
}

struct Other {
    let x: Int
}

extend Point: Greets {
    function greet(borrow self) -> String { return "hi" }
}

enum Crate<Held> {
    Full(Held)
    Empty
}

@Main
function main() {
    let point = Point(x: 1)
    let other = Other(x: 1)
    print(point.type == point.type)
    print(point.type == other.type)
    let erased: Any = Point(x: 2)
    print(erased.type == point.type)

    print(point.type.name)
    print(point.type.kind)
    print(point.type.package.count)
    print(point.type.conformances.count)
    print(point.type.conformances[0])

    let held: Crate<Int> = .Full(3)
    print(held.type.name)
    print(held.type.arguments.count)
    print(held.type.arguments[0].name)

    let words = ["a"]
    print(words.type.kind)
    print(words.type.arguments[0].name)

    let plain: Int = 3
    let narrow: U8 = 3
    print(plain.type == narrow.type)
    return
}
"#,
    );
    assert_eq!(
        output,
        "true\nfalse\ntrue\nPoint\nstruct\n0\n1\nGreets\nCrate\n1\nInt\narray\nString\ntrue\n"
    );
}

/// A failed cast written under `try` is a value the handler answers, and the
/// same cast without one still traps.
///
/// The two engines build the result differently — the VM branches over a stack
/// answer, native code branches over the same box tag in generated blocks — so
/// the payload, the failure, and the heap accounting all have to agree.
#[test]
fn a_tried_cast_answers_its_failure_on_every_backend() {
    let output = assert_parity_with_heap_balance(
        r#"
struct Point {
    let x: Int
}

@Main
function main() {
    let boxed: Any = Point(x: 7)
    attempt {
        let p = try boxed as Point
        print(p.x)
    } handle {
        Mismatch(actual) { print("unreachable: " + actual.name) }
    }

    let text: Any = "text"
    attempt {
        let q = try text as Point
        print(q.x)
    } handle {
        Mismatch(actual) { print(actual.name) }
    }

    let held: Any = 3
    attempt {
        let n = try held as Int
        print(n * 2)
    } handle {
        Mismatch(actual) { print(actual.name) }
    }
    return
}
"#,
    );
    assert_eq!(output, "7\nString\n6\n");
}

/// Serde arrays round-trip identically on every backend, nested and empty
/// included, with the heap balanced: every box the reader builds is released
/// with the value that holds it.
#[test]
fn serde_arrays_balance_on_every_backend() {
    let output = assert_parity_with_heap_balance(
        r#"
import Foundation

@Derive(Serializable, Deserializable)
struct Record {
    var code: Int
}

@Derive(Serializable, Deserializable)
struct Batch {
    var xs: [Int]
    var matrix: [[U8]]
    var records: [Record]
}

@Main
function main() {
    let batch = Batch(xs: [1, 2], matrix: [[U8(3)], []], records: [Record(code: 7)])
    let back = deserialize_Batch(serialize_Batch(batch))
    print(back.xs[1])
    print(back.matrix[0][0])
    print(back.matrix[1].count)
    print(back.records[0].code)
    return
}
"#,
    );
    assert_eq!(output, "2\n3\n0\n7\n");
}
