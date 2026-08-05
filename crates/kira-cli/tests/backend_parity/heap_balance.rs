//! The native heap balances, the way the VM's always has.
//!
//! The VM proves this on its own: its heap counts allocations and frees, and
//! `current == 0` at exit says every object it made came back. Native code had
//! no equivalent, and a struct case in `structs.rs` used to say so outright —
//! *"the VM proves its heap balances; the native backend has no such proof, so
//! this is what stands in for one"*. A native leak was invisible until
//! something ran out of memory.
//!
//! `KIRA_HEAP_REPORT` asks a native run for that proof: the runtime counts real
//! allocations and real frees — never a share bump, never an inline enum handle
//! that was never allocated — and the emitted `main` reports the balance just
//! before it returns. A hybrid program's native half is a shared library with
//! no `main`, so the host asks on its behalf when the run ends.
//!
//! These cases exist to be run *before* changing who emits a free. That is
//! their whole point: a rewrite of the drop logic either keeps them at zero or
//! is caught here rather than in production.

use std::process::Output;

use crate::{kira, write_source};

/// Runs `source` on `backend` with the heap report switched on.
fn run_with_report(source_path: &std::path::Path, backend: &str) -> Output {
    kira()
        .env("KIRA_HEAP_REPORT", "1")
        .args(["run", "--backend", backend, source_path.to_str().unwrap()])
        .output()
        .expect("run kira")
}

/// Asserts `source` allocates and frees the same number of objects.
///
/// Checked on the two backends that have a native half. The VM is not asked:
/// it proves its own balance internally and has no `kira_rt_*` heap to count.
fn assert_balances(source: &str) {
    let path = write_source(source);
    for backend in ["llvm", "hybrid"] {
        let run = run_with_report(&path, backend);
        let stderr = String::from_utf8_lossy(&run.stderr).into_owned();
        assert!(
            stderr.contains("live=0"),
            "the {backend} backend did not balance its heap:\n{stderr}"
        );
        assert!(
            !stderr.contains("leaked"),
            "the {backend} backend leaked:\n{stderr}"
        );
        assert_eq!(
            run.status.code(),
            Some(0),
            "an unbalanced run exits non-zero; {backend} stderr:\n{stderr}"
        );
    }
    let _ = std::fs::remove_dir_all(path.parent().expect("program directory"));
}

/// The report is actually produced, and says it is counting.
///
/// Guards the case that would make every other assertion here vacuous: a build
/// that stopped counting reports `live=0` because it measured nothing, and
/// would pass a balance check while proving nothing at all.
#[test]
fn a_native_run_reports_a_balance_it_actually_measured() {
    let path = write_source(
        r#"
@Main
function main() {
    let text = "counted"
    print(text)
    return
}
"#,
    );
    let run = run_with_report(&path, "llvm");
    let stderr = String::from_utf8_lossy(&run.stderr).into_owned();
    let _ = std::fs::remove_dir_all(path.parent().expect("program directory"));

    assert!(
        stderr.contains("heap allocated="),
        "the report must be emitted when asked for:\n{stderr}"
    );
    assert!(
        !stderr.contains("not compiled into this build"),
        "a test build must be counting, or every balance assertion is vacuous:\n{stderr}"
    );
    // One string, allocated and released — a run that counted nothing would
    // report zero allocations and still say `live=0`.
    assert!(
        stderr.contains("allocated=1"),
        "the one string this program makes must be counted:\n{stderr}"
    );
}

/// Strings in a loop balance: the case where a missing free compounds.
#[test]
fn strings_made_in_a_loop_are_all_released() {
    assert_balances(
        r#"
@Main
function main() {
    var i = 0
    while i < 5 {
        let text = "x" + "y"
        print(text)
        i = i + 1
    }
    return
}
"#,
    );
}

/// A struct holding a string, copied and overwritten, balances.
///
/// Copying is where a shallow copy would double-free and a missing copy would
/// leak, so the count is the thing that tells them apart.
#[test]
fn structs_carrying_strings_balance() {
    assert_balances(
        r#"
struct Item {
    var name: String = ""
    var n: Int = 0
}

@Main
function main() {
    var acc = Item { name: "start", n: 0 }
    var i = 0
    while i < 4 {
        var next = Item { name: "x", n: i }
        next.name = next.name + "y"
        acc = next
        i = i + 1
    }
    print(acc.name)
    return
}
"#,
    );
}

/// Arrays and enums with owned payloads balance.
#[test]
fn arrays_and_enum_payloads_balance() {
    assert_balances(
        r#"
enum Outcome {
    Ok
    Failed(String)
}

@Main
function main() {
    let names = ["a", "b", "c"]
    for name in names {
        print(name)
    }
    var i = 0
    while i < 3 {
        let outcome = Outcome.Failed("bad")
        match outcome {
            Ok -> print("ok");
            Failed(reason) -> print(reason);
        }
        i = i + 1
    }
    return
}
"#,
    );
}

/// Values crossing the `@Native` seam balance, in both directions.
///
/// The seam is where ownership is handed over rather than merely tracked, so a
/// leak there is a rule that was written down wrong rather than a free that was
/// forgotten. A struct, an array and an enum payload all cross as a node tree
/// that the reader frees; this is what says that free happens exactly once.
#[test]
fn values_crossing_the_native_seam_balance() {
    assert_balances(
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
function point_label(point: borrow Point) -> String {
    return point.label
}

@Native
function make_numbers() -> [Int] {
    return [1, 2, 3]
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
        Ok -> return "ok";
        Failed(reason) -> return reason;
    }
    return "?"
}

@Main
function main() {
    var i = 0
    while i < 3 {
        let point = make_point(i)
        print(point_label(point))
        let numbers = make_numbers()
        print(sum_numbers(numbers))
        print(describe(classify(i)))
        i = i + 1
    }
    return
}
"#,
    );
}

/// The native half of the release plan, on the slots the two engines answer
/// differently about.
///
/// `kira_ir::mid` plans releases for both engines and is told how each lends a
/// borrowed parameter: native passes a pointer into the caller's frame, the VM
/// passes a copy the callee owns. Given the wrong answer, native frees a value
/// its caller still holds — which this counts as a double free, and the VM's
/// side of the same program counts as a leak in `kira-build`'s release tests.
#[test]
fn a_mutable_string_borrow_balances_natively() {
    assert_balances(
        r#"
struct Note {
    var body: String
}

function retitle(n: borrow mut Note, to: String) {
    n.body = to + "!"
    return
}

function grow(text: borrow mut String) {
    text = text + "+"
    return
}

function label(n: borrow Note) -> String {
    let copy = n.body
    return copy
}

@Main
function main() {
    var note = Note { body: "first" }
    var i = 0
    while i < 3 {
        retitle(note, "round")
        print(label(note))
        i = i + 1
    }

    var word = "a"
    var j = 0
    while j < 3 {
        grow(word)
        j = j + 1
    }
    print(word)
    return
}
"#,
    );
}
