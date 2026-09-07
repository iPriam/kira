//! Parity across the hybrid seam: crossings, traps, and bridge types.

use crate::{assert_parity, assert_parity_with_heap_balance, assert_trap_parity};

/// The simplest crossing: the VM half reaches a native callee and gets a value
/// back.
#[test]
fn a_runtime_caller_reaching_a_native_callee_agrees() {
    let output = assert_parity(
        r#"
@Native
function double(n: Int) -> Int {
    return n * 2
}

@Main
function main() {
    print(double(21))
    print(double(-1))
    return
}
"#,
    );
    assert_eq!(output, "42\n-2\n");
}

/// An `extend` modifier is a synthesized body, and `@Native` on it puts that
/// body in the native half exactly as it does for a written function. A hybrid
/// run is what proves it: the modifier is called from the VM half, so a
/// modifier that stayed on the VM and one that crossed print the same number
/// only if the crossing works.
#[test]
fn a_native_extend_modifier_crosses_the_seam() {
    let output = assert_parity(
        r#"
construct Widget {
    let tag: Int { 0 }
}

construct Leaf(id: Int) extends Widget {
    let tag: Int { id }
}

extend Widget {
    @Native function doubled() -> Int {
        return self.tag * 2
    }
    @Runtime function tripled() -> Int {
        return self.tag * 3
    }
}

@Main
function main() {
    let base = Leaf(id: 7)
    print(base.doubled())
    print(base.tripled())
    return
}
"#,
    );
    assert_eq!(output, "14\n21\n");
}

/// The other direction: native code calls back into the VM through the invoker
/// the host installs.
#[test]
fn a_native_caller_reaching_a_runtime_callee_agrees() {
    let output = assert_parity(
        r#"
@Runtime
function add_one(n: Int) -> Int {
    return n + 1
}

@Native
function twice_plus_one(n: Int) -> Int {
    return add_one(n) * 2
}

@Main
function main() {
    print(twice_plus_one(20))
    return
}
"#,
    );
    assert_eq!(output, "42\n");
}

/// `Any` crosses directly in both seam directions. The native caller invokes
/// the runtime callee, while the runtime caller invokes the native callee, so
/// both the argument tree and the returned tree are exercised.
#[test]
fn any_crosses_directly_in_both_seam_directions() {
    let output = assert_parity_with_heap_balance(
        r#"
@Native
function native_echo(value: Any) -> Any {
    return value
}

@Runtime
function runtime_echo(value: Any) -> Any {
    return value
}

@Native
function native_calls_runtime(value: Any) -> Any {
    return runtime_echo(move value)
}

@Runtime
function runtime_calls_native(value: Any) -> Any {
    return native_echo(move value)
}

@Main
@Runtime
function main() {
    let expectedNumber: Any = 7
    let expectedText: Any = "runtime"
    print(runtime_calls_native(7) == expectedNumber)
    print(native_calls_runtime("runtime") == expectedText)
    return
}
"#,
    );
    assert_eq!(output, "true\ntrue\n");
}

/// An `Any` nested in a struct and in two array levels survives both seam
/// directions. Comparing the returned fields proves the recursive payload was
/// rebuilt rather than merely carrying an opaque root handle.
#[test]
fn nested_any_crosses_both_seam_directions_and_releases() {
    let output = assert_parity_with_heap_balance(
        r#"
struct Envelope {
    let direct: Any
    let layers: [[Any]]
}

@Runtime
function runtime_envelope(value: Envelope) -> Envelope {
    return value
}

@Native
function native_through_runtime(value: Envelope) -> Envelope {
    return runtime_envelope(move value)
}

@Native
function native_envelope(value: Envelope) -> Envelope {
    return value
}

@Runtime
function runtime_through_native(value: Envelope) -> Envelope {
    return native_envelope(move value)
}

function envelope() -> Envelope {
    return Envelope {
        direct: "payload",
        layers: [[1, "one"], [true, 3.5]]
    }
}

@Main
@Runtime
function main() {
    let first = native_through_runtime(envelope())
    let second = runtime_through_native(envelope())
    let text: Any = "payload"
    let one: Any = "one"
    let truth: Any = true
    print(first.direct == text)
    print(first.layers[0][1] == one)
    print(first.layers[1][0] == truth)
    print(second.direct == text)
    print(second.layers[0][1] == one)
    print(second.layers[1][0] == truth)
    return
}
"#,
    );
    assert_eq!(output, "true\ntrue\ntrue\ntrue\ntrue\ntrue\n");
}

#[test]
fn any_aggregate_payloads_cross_both_seams_with_their_dynamic_identity() {
    let output = assert_parity_with_heap_balance(
        r#"
struct Point {
    let x: Int
    let label: String
}

enum Choice {
    Point(Point)
    Empty
}

@Native
function native_echo(value: Any) -> Any {
    return value
}

@Runtime
function runtime_echo(value: Any) -> Any {
    return value
}

@Native
function native_calls_runtime(value: Any) -> Any {
    return runtime_echo(move value)
}

@Runtime
function runtime_calls_native(value: Any) -> Any {
    return native_echo(move value)
}

@Main
@Runtime
function main() {
    let expectedPoint: Any = Point(x: 3, label: "point")
    let expectedNumbers: Any = [1, 2, 3]
    let expectedChoice: Any = Choice.Point(Point(x: 4, label: "choice"))
    print(runtime_calls_native(Point(x: 3, label: "point")) == expectedPoint)
    print(native_calls_runtime([1, 2, 3]) == expectedNumbers)
    print(runtime_calls_native(Choice.Point(Point(x: 4, label: "choice"))) == expectedChoice)
    return
}
"#,
    );
    assert_eq!(output, "true\ntrue\ntrue\n");
}

/// A string crossing into native code and back out again. This is the case the
/// seam's ownership rules govern: the callee frees the argument handle, and the
/// host frees the result handle. Getting either wrong is a double free or a
/// leak, and doing it twice in a row is what surfaces a double free.
///
/// The empty string is the interesting third case: it crosses as a *null*
/// handle, which every runtime helper accepts as the empty string — so a `""`
/// argument must produce `"!"` and not a crash.
#[test]
fn a_string_crossing_into_native_code_and_back_agrees() {
    let output = assert_parity(
        r#"
@Native
function shout(text: String) -> String {
    return text + "!"
}

@Main
function main() {
    print(shout("hello"))
    print(shout("world"))
    print(shout(""))
    return
}
"#,
    );
    assert_eq!(output, "hello!\nworld!\n!\n");
}

/// A string crossing the other way: native code hands one to the VM and takes
/// the result back. The invoker frees the arguments it is given and allocates
/// the result out of the library's own allocator — the host's copy of those
/// symbols would be a cross-allocator free.
#[test]
fn a_string_crossing_into_runtime_code_and_back_agrees() {
    let output = assert_parity(
        r#"
@Runtime
function greet(name: borrow String) -> String {
    return "hi, " + name
}

@Native
function loud(name: String) -> String {
    return greet(name) + "!"
}

@Main
function main() {
    print(loud("kira"))
    print(loud("again"))
    return
}
"#,
    );
    assert_eq!(output, "hi, kira!\nhi, again!\n");
}

/// A trap on the native side of the boundary.
#[test]
fn a_trap_in_native_code_traps_on_every_backend() {
    assert_trap_parity(
        r#"
@Native
function divide(n: Int, by: Int) -> Int {
    return n / by
}

@Main
function main() {
    print(1)
    print(divide(10, 0))
    return
}
"#,
        "1\n",
    );
}

/// A trap on the runtime side, reached *through* native code. The VM's trap has
/// nowhere to return to across the C frame, so the invoker reports and exits —
/// and a user must not be able to tell which side of the boundary it happened
/// on.
#[test]
fn a_trap_in_runtime_code_reached_through_native_code_traps_on_every_backend() {
    assert_trap_parity(
        r#"
@Runtime
function divide(n: Int, by: Int) -> Int {
    return n / by
}

@Native
function reach(n: Int) -> Int {
    return divide(n, 0)
}

@Main
function main() {
    print(1)
    print(reach(10))
    return
}
"#,
        "1\n",
    );
}

/// `@Main` is annotatable like anything else, so the entrypoint itself can be
/// native — and then the whole program starts in the library and reaches back.
#[test]
fn a_native_entrypoint_agrees() {
    let output = assert_parity(
        r#"
@Runtime
function helper(n: Int) -> Int {
    return n + 1
}

@Main @Native
function main() {
    print(helper(41))
    return
}
"#,
    );
    assert_eq!(output, "42\n");
}

/// Calls nest across the boundary in both directions at once. Each crossing
/// runs the VM on its own heap and operand stack, which is what lets a native
/// function call a runtime function that calls a native one.
#[test]
fn calls_nesting_across_the_boundary_agree() {
    let output = assert_parity(
        r#"
@Native
function inner(n: Int) -> Int {
    return n * 2
}

@Runtime
function middle(n: Int) -> Int {
    return inner(n) + 1
}

@Native
function outer(n: Int) -> Int {
    return middle(n) * 10
}

@Main
function main() {
    print(outer(5))
    return
}
"#,
    );
    assert_eq!(output, "110\n");
}

/// Recursion through the boundary: every crossing is a fresh nesting level, so
/// a recursive call that alternates engines must not lose a frame.
#[test]
fn recursion_across_the_boundary_agrees() {
    let output = assert_parity(
        r#"
@Runtime
function count_down(n: Int) -> Int {
    if n <= 0 {
        return 0
    }
    return step(n)
}

@Native
function step(n: Int) -> Int {
    return count_down(n - 1) + n
}

@Main
function main() {
    print(count_down(10))
    return
}
"#,
    );
    assert_eq!(output, "55\n");
}

/// Every value the seam can carry, across it and back, in one program.
#[test]
fn every_bridge_type_crosses_the_boundary_intact() {
    let output = assert_parity(
        r#"
@Native
function round_trip_int(value: Int) -> Int {
    return value
}

@Native
function round_trip_float(value: Float) -> Float {
    return value
}

@Native
function round_trip_bool(value: Bool) -> Bool {
    return value
}

@Native
function round_trip_string(value: String) -> String {
    return value
}

@Main
function main() {
    print(round_trip_int(-9223372036854775807))
    print(round_trip_float(2.0))
    print(round_trip_float(0.5))
    print(round_trip_bool(true))
    print(round_trip_bool(false))
    print(round_trip_string("intact"))
    return
}
"#,
    );
    assert_eq!(
        output,
        "-9223372036854775807\n2\n0.5\ntrue\nfalse\nintact\n"
    );
}

// ----- aggregates ------------------------------------------------------
//
// Enums, structs and arrays cross by two different mechanisms, and which one a
// value takes is not visible in the program that uses it — so both are tested
// through ordinary programs rather than by asserting on the encoding.
//
// A payload-less enum crosses as a bare variant tag, with nothing allocated on
// either side. Everything else — a struct, an array, an enum carrying a payload
// — crosses as a node tree that is built, transferred, and freed by the reader.
// The seam that picks between them is the thing under test.

/// A payload-less enum crosses in both directions, on every backend.
///
/// The value is its variant tag and nothing else, so neither side's
/// representation travels — the VM holds an index into its heap and native
/// holds `(tag << 1) | 1` inline, and only the number between them crosses.
#[test]
fn a_payload_less_enum_crosses_the_boundary_in_both_directions() {
    let output = assert_parity(
        r#"
enum Engine {
    Vm
    Native
    Hybrid
}

@Native
function pick_engine(index: Int) -> Engine {
    if index == 0 {
        return Engine.Vm
    }
    if index == 1 {
        return Engine.Native
    }
    return Engine.Hybrid
}

@Runtime
function name_engine(engine: Engine) -> String {
    if engine == .Vm {
        return "vm"
    }
    if engine == .Native {
        return "native"
    }
    return "hybrid"
}

@Native
function round_trip_engine(engine: Engine) -> Engine {
    return engine
}

@Main
function main() {
    print(name_engine(pick_engine(0)))
    print(name_engine(pick_engine(1)))
    print(name_engine(pick_engine(2)))
    print(name_engine(round_trip_engine(pick_engine(2))))
    return
}
"#,
    );
    assert_eq!(output, "vm\nnative\nhybrid\nhybrid\n");
}

/// The last variant's tag survives, which a truncating encode would lose.
///
/// The tag rides in a signed 64-bit payload and is rebuilt as an unsigned
/// index. A wrong tag is a wrong *variant* — a silently different program, not
/// a crash — so the highest tag in the enum is the one worth pinning.
#[test]
fn the_highest_variant_tag_survives_the_crossing() {
    let output = assert_parity(
        r#"
enum Step {
    A
    B
    C
    D
    E
}

@Native
function last_step() -> Step {
    return Step.E
}

@Main
function main() {
    let step = last_step()
    if step == .E {
        print("E")
    }
    if step == .A {
        print("A")
    }
    return
}
"#,
    );
    assert_eq!(output, "E\n");
}

/// A struct, an array, and a payload-carrying enum all cross, both ways.
///
/// All three take the same route — a node tree, transferred to the reader,
/// which frees it as it decodes — so they are tested together: what would break
/// one would break all three, and a single case would not say which.
///
/// The string inside the struct and the string inside the enum payload are the
/// point. A tree of scalars proves the shape crosses; a tree with owned storage
/// in it proves the ownership answer, which is the thing that was undecided.
#[test]
fn structs_arrays_and_payload_enums_cross_the_boundary() {
    let output = assert_parity(
        r#"
struct Point {
    var x: Int = 0
    var label: String = ""
}

enum Outcome {
    Ok
    Failed(String)
}

@Native
function make_point(x: Int) -> Point {
    return Point { x: x, label: "made" }
}

@Native
function point_x(point: borrow Point) -> Int {
    return point.x
}

@Native
function point_label(point: borrow Point) -> String {
    return point.label
}

@Native
function make_numbers() -> [Int] {
    return [10, 20, 30]
}

@Native
function sum_numbers(values: borrow [Int]) -> Int {
    var total = 0
    for value in values {
        total = total + value
    }
    return total
}

@Native
function classify(code: Int) -> Outcome {
    if code == 0 {
        return Outcome.Ok
    }
    return Outcome.Failed("not zero")
}

@Native
function describe(outcome: borrow Outcome) -> String {
    match outcome {
        Ok -> return "ok"
        Failed(reason) -> return reason
    }
    return "?"
}

@Main
function main() {
    let point = make_point(7)
    print(point_x(point))
    print(point_label(point))

    let numbers = make_numbers()
    print(sum_numbers(numbers))
    print(sum_numbers([1, 2, 3, 4]))

    print(describe(classify(0)))
    print(describe(classify(1)))
    return
}
"#,
    );
    assert_eq!(output, "7\nmade\n60\n10\nok\nnot zero\n");
}

/// A tree survives nesting, which a one-level copy would flatten or lose.
///
/// A struct holding an array of structs is the shape that catches an encoder
/// that only recurses once — and the empty array is the case that catches a
/// length written before it is known.
#[test]
fn a_nested_aggregate_crosses_whole() {
    let output = assert_parity(
        r#"
struct Tag {
    var name: String = ""
}

struct Bag {
    var tags: [Tag] = []
    var count: Int = 0
}

@Native
function make_bag() -> Bag {
    return Bag { tags: [Tag { name: "a" }, Tag { name: "b" }], count: 2 }
}

@Native
function empty_bag() -> Bag {
    return Bag { tags: [], count: 0 }
}

@Native
function first_tag(bag: borrow Bag) -> String {
    for tag in bag.tags {
        return tag.name
    }
    return "none"
}

@Native
function bag_count(bag: borrow Bag) -> Int {
    return bag.count
}

@Main
function main() {
    let bag = make_bag()
    print(first_tag(bag))
    print(bag_count(bag))

    let empty = empty_bag()
    print(first_tag(empty))
    print(bag_count(empty))
    return
}
"#,
    );
    assert_eq!(output, "a\n2\nnone\n0\n");
}

// ----- structs ---------------------------------------------------------
//
// A struct is a value, and these cases are about what that costs each backend
// to honour. The VM copies a heap object on every read; the native backend
// copies an LLVM struct field by field. Those are different mechanisms for one
// rule, which is exactly the kind of pair that drifts silently — so the rule is
// tested rather than assumed.

/// A `borrow mut` struct parameter crossing into the native half.
///
/// The write lands in the caller's storage, which within one engine is a
/// pointer and across the seam cannot be: the VM's value lives in a heap
/// machine code has no address in. So the value goes over as a copy and the
/// callee's final value comes back in the slot it arrived in. What the program
/// observes has to be identical either way, which is what this compares.
#[test]
fn a_native_callee_writing_through_a_struct_parameter_agrees() {
    let output = assert_parity(
        r#"
struct Point {
    var x: Int
    var label: String
}

@Native
function shift(p: borrow mut Point, by: Int) -> Int {
    p.x = p.x + by
    p.label = p.label + "!"
    return p.x
}

@Main
function main() {
    var q = Point { x = 3, label = "q" }
    let answer = shift(q, 10)
    print(q.x)
    print(q.label)
    print(answer)
    return
}
"#,
    );
    assert_eq!(output, "13\nq!\n13\n");
}

/// The same crossing in the other direction: machine code writing through a
/// parameter of a `@Runtime` callee.
#[test]
fn a_runtime_callee_writing_through_a_struct_parameter_agrees() {
    let output = assert_parity(
        r#"
struct Point {
    var x: Int
    var label: String
}

@Runtime
function shift(p: borrow mut Point, by: Int) -> Int {
    p.x = p.x + by
    p.label = p.label + "!"
    return p.x
}

@Native
function drive() -> Int {
    var q = Point { x = 3, label = "q" }
    let answer = shift(q, 10)
    print(q.x)
    print(q.label)
    return answer
}

@Main
function main() {
    print(drive())
    return
}
"#,
    );
    assert_eq!(output, "13\nq!\n13\n");
}

/// Everything a written-through struct can hold, at once: a nested struct, a
/// string, an array grown by the callee, and two mutable parameters in one
/// call. A tree that dropped a level or an ownership slip inside one shows up
/// here as a divergence rather than as a leak nobody looks for.
#[test]
fn a_deep_written_through_struct_agrees() {
    let output = assert_parity(
        r#"
struct Inner {
    var tag: String
    var n: Int
}

struct Outer {
    var inner: Inner
    var items: [Int]
    var name: String
}

@Native
function rework(o: borrow mut Outer, extra: borrow mut Inner, by: Int) -> Int {
    o.inner.n = o.inner.n + by
    o.name = o.name + "!"
    o.items.append(by)
    extra.tag = extra.tag + "?"
    extra.n = extra.n * 2
    return o.inner.n + extra.n
}

@Main
function main() {
    var o = Outer {
        inner = Inner { tag = "i", n = 1 },
        items = [7],
        name = "o"
    }
    var e = Inner { tag = "e", n = 5 }
    let answer = rework(o, e, 10)
    print(o.name)
    print(o.inner.n)
    print(o.items.count)
    print(o.items[1])
    print(e.tag)
    print(e.n)
    print(answer)
    return
}
"#,
    );
    assert_eq!(output, "o!\n11\n2\n10\ne?\n10\n21\n");
}
