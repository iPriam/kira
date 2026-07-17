//! Parity across the hybrid seam: crossings, traps, and bridge types.

use crate::{assert_parity, assert_trap_parity};

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
function greet(name: String) -> String {
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

// ----- structs ---------------------------------------------------------
//
// A struct is a value, and these cases are about what that costs each backend
// to honour. The VM copies a heap object on every read; the native backend
// copies an LLVM struct field by field. Those are different mechanisms for one
// rule, which is exactly the kind of pair that drifts silently — so the rule is
// tested rather than assumed.
