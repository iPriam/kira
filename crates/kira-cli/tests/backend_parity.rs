//! Differential tests: the VM and the LLVM/native backend must not disagree.
//!
//! Parity is proven, not asserted. Each case compiles one program through both
//! backends from the same IR and requires identical program output and exit
//! status — that is the whole contract a Kira user sees, so a divergence here
//! is a real bug in one of the two.
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

/// Asserts the VM and the native backend agree on `source`, returning the
/// output both produced.
fn assert_parity(source: &str) -> String {
    let path = write_source(source);
    let vm = run_on(&path, "vm");
    let native = run_on(&path, "llvm");
    let _ = std::fs::remove_dir_all(path.parent().expect("program directory"));

    let vm_stdout = String::from_utf8_lossy(&vm.stdout).into_owned();
    let native_stdout = String::from_utf8_lossy(&native.stdout).into_owned();
    assert_eq!(
        vm_stdout,
        native_stdout,
        "VM and native output differ.\nvm stderr: {}\nnative stderr: {}",
        String::from_utf8_lossy(&vm.stderr),
        String::from_utf8_lossy(&native.stderr),
    );
    assert_eq!(
        vm.status.code(),
        native.status.code(),
        "VM and native exit codes differ for:\n{source}",
    );
    vm_stdout
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

/// Division by zero is a trap in Kira, not UB: both backends must refuse it the
/// same way — no program output, non-zero exit.
#[test]
fn division_by_zero_traps_on_both_backends() {
    let path = write_source(
        r#"
@Main
function main() {
    var zero = 0
    print(1)
    print(10 / zero)
    return
}
"#,
    );
    let vm = run_on(&path, "vm");
    let native = run_on(&path, "llvm");
    let _ = std::fs::remove_dir_all(path.parent().expect("program directory"));

    // The output produced before the trap is kept; the trap itself is not
    // reported on stdout, and neither run succeeds.
    assert_eq!(String::from_utf8_lossy(&vm.stdout), "1\n");
    assert_eq!(String::from_utf8_lossy(&native.stdout), "1\n");
    assert_eq!(vm.status.code(), native.status.code());
    assert_ne!(vm.status.code(), Some(0), "a trap must not report success");
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

/// Every example in the repo must behave identically on both backends.
#[test]
fn every_example_agrees_on_both_backends() {
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
            let native = run_on(&source, "llvm");
            assert_eq!(
                String::from_utf8_lossy(&vm.stdout),
                String::from_utf8_lossy(&native.stdout),
                "example `{}` differs between backends.\nnative stderr: {}",
                source.display(),
                String::from_utf8_lossy(&native.stderr),
            );
            assert_eq!(vm.status.code(), native.status.code());
            checked += 1;
        }
    }
    assert!(checked > 0, "no examples were checked");
}
