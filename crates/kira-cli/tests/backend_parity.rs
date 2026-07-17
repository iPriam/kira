//! Differential tests: the VM, the LLVM/native backend, and the hybrid bundle
//! must not disagree.
//!
//! Parity is proven, not asserted. Each case compiles one program through every
//! backend from the same IR and requires identical program output and exit
//! status — that is the whole contract a Kira user sees, so a divergence here
//! is a real bug in one of them.
//!
//! # What each backend does with an annotation
//!
//! `--backend vm` compiles every function to bytecode and `--backend llvm`
//! makes every function native: an execution boundary needs two engines, and
//! these builds have one, so both ignore `@Runtime`/`@Native` entirely. Only
//! `--backend hybrid` splits a program on them. That is what makes these three
//! comparable on *any* program: the annotations change where code runs without
//! changing what it does, and a case here that says otherwise is a bug.
//!
//! So an unannotated case exercises no crossing on any backend, hybrid
//! included — it compiles to a single engine like the other two. The annotated
//! cases are the ones that build a real boundary, and they are still parity
//! tests: agreeing with `vm` and `llvm`, which ignored the annotations, is the
//! statement that a boundary changed where the code ran and nothing else.
//!
//! These only run when `kirac` was built with its `llvm` feature; without it
//! there is no native backend to compare against.
#![cfg(feature = "llvm")]

use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU32, Ordering};

/// Writes `source` to a uniquely-named temp `.kira` file.
///
/// Each program gets its own directory: `.kira-build` artifacts land beside the
/// source, and tests run in parallel.
fn write_source(source: &str) -> PathBuf {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    let directory = std::env::temp_dir().join(format!("kirac_parity_{pid}_{unique}"));
    std::fs::create_dir_all(&directory).expect("temp dir");
    let path = directory.join("program.kira");
    std::fs::write(&path, source).expect("write temp source");
    path
}

/// Runs `source` on one backend.
fn run_on(source_path: &std::path::Path, backend: &str) -> Output {
    Command::new(env!("CARGO_BIN_EXE_kirac"))
        .args(["run", "--backend", backend, source_path.to_str().unwrap()])
        .output()
        .expect("run kirac")
}

/// Every backend a program must behave identically on.
const BACKENDS: [&str; 3] = ["vm", "llvm", "hybrid"];

/// Asserts every backend agrees on `source`, returning the output they produced.
///
/// The VM is the reference: it is the simplest of the three and the one whose
/// semantics the other two are defined to mirror, so a disagreement names the
/// backend that drifted rather than leaving two answers to choose between.
fn assert_parity(source: &str) -> String {
    let path = write_source(source);
    let runs: Vec<(&str, Output)> = BACKENDS
        .iter()
        .map(|backend| (*backend, run_on(&path, backend)))
        .collect();
    let _ = std::fs::remove_dir_all(path.parent().expect("program directory"));

    let (_, reference) = &runs[0];
    let expected = String::from_utf8_lossy(&reference.stdout).into_owned();

    for (backend, run) in &runs[1..] {
        let stdout = String::from_utf8_lossy(&run.stdout).into_owned();
        assert_eq!(
            expected,
            stdout,
            "the vm and {backend} backends disagree on output for:\n{source}\n\
             vm stderr: {}\n{backend} stderr: {}",
            String::from_utf8_lossy(&reference.stderr),
            String::from_utf8_lossy(&run.stderr),
        );
        assert_eq!(
            reference.status.code(),
            run.status.code(),
            "the vm and {backend} backends disagree on exit code for:\n{source}\n\
             {backend} stderr: {}",
            String::from_utf8_lossy(&run.stderr),
        );
    }
    expected
}

/// Asserts every backend refuses `source` the same way: no output, non-zero
/// exit.
///
/// A trap is the one case where stdout alone would prove too little — a program
/// that printed nothing and exited cleanly would pass a stdout comparison.
fn assert_trap_parity(source: &str, before_the_trap: &str) {
    let path = write_source(source);
    for backend in BACKENDS {
        let run = run_on(&path, backend);
        assert_eq!(
            String::from_utf8_lossy(&run.stdout),
            before_the_trap,
            "the {backend} backend printed something other than the output \
             preceding the trap for:\n{source}",
        );
        assert_eq!(
            run.status.code(),
            Some(1),
            "the {backend} backend did not trap for:\n{source}\nstderr: {}",
            String::from_utf8_lossy(&run.stderr),
        );
    }
    let _ = std::fs::remove_dir_all(path.parent().expect("program directory"));
}

#[test]
fn arithmetic_and_integer_division_agree() {
    let output = assert_parity(
        r#"
@Main
function main() {
    print(1 + 2 * 3 - 4)
    print(7 / 2)
    print(-7 % 2)
    print(17 % 5)
    print(-(3 + 4))
    return
}
"#,
    );
    assert_eq!(output, "3\n3\n-1\n2\n-7\n");
}

/// The case LLVM would get wrong on its own: `sdiv i64 MIN, -1` is poison, but
/// the VM's `wrapping_div` defines it as `MIN`. The backend branches around it,
/// and this proves the branch is really there.
#[test]
fn integer_overflow_in_division_wraps_like_the_vm() {
    let output = assert_parity(
        r#"
@Main
function main() {
    var min = -9223372036854775807
    min = min - 1
    print(min / -1)
    print(min % -1)
    return
}
"#,
    );
    assert_eq!(output, "-9223372036854775808\n0\n");
}

/// Signed arithmetic wraps rather than trapping or being poison, matching the
/// VM's `wrapping_*` operators.
#[test]
fn signed_arithmetic_wraps_on_overflow() {
    let output = assert_parity(
        r#"
@Main
function main() {
    var max = 9223372036854775807
    print(max + 1)
    var min = -9223372036854775807
    min = min - 1
    print(min - 1)
    return
}
"#,
    );
    assert_eq!(output, "-9223372036854775808\n9223372036854775807\n");
}

/// Division by zero is a trap in Kira, not UB: every backend must refuse it the
/// same way — the output before the trap is kept, the trap itself reaches no
/// stdout, and no run succeeds.
#[test]
fn division_by_zero_traps_on_every_backend() {
    assert_trap_parity(
        r#"
@Main
function main() {
    var zero = 0
    print(1)
    print(10 / zero)
    return
}
"#,
        "1\n",
    );
}

/// Float formatting is where a hand-written native runtime would drift from the
/// VM. Both format with the same standard library, so a whole float prints
/// without a decimal point on both.
#[test]
fn float_arithmetic_and_formatting_agree() {
    let output = assert_parity(
        r#"
@Main
function main() {
    let a = 1.5
    let b = 2.0
    print(a + b)
    print(b)
    print(a * b)
    print(a / b)
    print(a < b)
    print(b == 2.0)
    print(-a)
    return
}
"#,
    );
    assert_eq!(output, "3.5\n2\n3\n0.75\ntrue\ntrue\n-1.5\n");
}

#[test]
fn booleans_and_short_circuit_operators_agree() {
    let output = assert_parity(
        r#"
@Main
function main() {
    let yes = true
    let no = false
    print(yes && no)
    print(yes || no)
    print(!yes)
    print(yes == true)
    print(1 < 2 && 3 >= 3)
    return
}
"#,
    );
    assert_eq!(output, "false\ntrue\nfalse\ntrue\ntrue\n");
}

/// `&&` must not evaluate its right operand when the left already decides the
/// answer: the call would trap, so reaching it changes the exit status on
/// whichever backend got it wrong.
#[test]
fn short_circuit_skips_the_right_operand() {
    let output = assert_parity(
        r#"
@Main
function main() {
    var zero = 0
    if false && boom(zero) {
        print(999)
    }
    print(1)
    return
}

function boom(zero: Int) -> Bool {
    return 1 / zero == 0
}
"#,
    );
    assert_eq!(output, "1\n", "the trapping operand must never run");
}

#[test]
fn strings_concatenate_compare_and_return_identically() {
    let output = assert_parity(
        r#"
@Main
function main() {
    let greeting = "hello"
    let subject = "kira"
    print(greeting + " " + subject)
    print(banner())
    print(greeting == "hello")
    print(greeting == subject)
    print(greeting != subject)
    print("")
    return
}

function banner() -> String {
    return "one source" + ", many backends"
}
"#,
    );
    assert_eq!(
        output,
        "hello kira\none source, many backends\ntrue\nfalse\ntrue\n\n"
    );
}

/// A `let` inside a loop stores into the same slot every iteration; both
/// backends must reclaim the previous value rather than leak or double-free it.
#[test]
fn strings_rebound_in_a_loop_agree() {
    let output = assert_parity(
        r#"
@Main
function main() {
    var i = 0
    var acc = ""
    while i < 3 {
        let piece = "x"
        acc = acc + piece
        i = i + 1
    }
    print(acc)
    return
}
"#,
    );
    assert_eq!(output, "xxx\n");
}

#[test]
fn recursion_and_control_flow_agree() {
    let output = assert_parity(
        r#"
@Main
function main() {
    print(fib(20))
    var i = 0
    var sum = 0
    while i < 10 {
        sum = sum + i
        i = i + 1
    }
    print(sum)
    if sum > 40 {
        print(sum > 40 && sum < 50)
    } else {
        print(false)
    }
    return
}

function fib(n: Int) -> Int {
    if n < 2 {
        return n
    }
    return fib(n - 1) + fib(n - 2)
}
"#,
    );
    assert_eq!(output, "6765\n45\ntrue\n");
}

/// Every example in the repo must behave identically on every backend.
#[test]
fn every_example_agrees_on_every_backend() {
    let examples = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples")
        .canonicalize()
        .expect("the examples directory");

    let mut checked = 0;
    for entry in std::fs::read_dir(&examples).expect("read examples") {
        let directory = entry.expect("example entry").path();
        if !directory.is_dir() {
            continue;
        }
        for file in std::fs::read_dir(&directory).expect("read example directory") {
            let source = file.expect("example file").path();
            if source.extension().is_none_or(|kind| kind != "kira") {
                continue;
            }
            let vm = run_on(&source, "vm");
            for backend in &BACKENDS[1..] {
                let run = run_on(&source, backend);
                assert_eq!(
                    String::from_utf8_lossy(&vm.stdout),
                    String::from_utf8_lossy(&run.stdout),
                    "example `{}` differs between the vm and {backend} backends.\n\
                     {backend} stderr: {}",
                    source.display(),
                    String::from_utf8_lossy(&run.stderr),
                );
                assert_eq!(
                    vm.status.code(),
                    run.status.code(),
                    "example `{}` exits differently on the vm and {backend} backends",
                    source.display(),
                );
            }
            checked += 1;
        }
    }
    assert!(checked > 0, "no examples were checked");
}

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

function sum(v: Vec3) -> Int {
    return v.x + v.y + v.z
}

function scaled(v: Vec3, k: Int) -> Vec3 {
    return Vec3 { x = v.x * k, y = v.y * k, z = v.z * k }
}

function bump(v: Vec3) -> Int {
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

function local_only(p: Point) -> Int {
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
