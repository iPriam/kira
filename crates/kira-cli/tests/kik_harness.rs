//! The in-depth stress harness, run through the real binary.
//!
//! `tests-kik/harness` is one Kira package that exercises the executable
//! surface in depth: fifteen areas, over a thousand `Test` declarations, and a
//! `@Main` that reduces every area to a checksum. It is the corpus the
//! reference implementation used as its own stress suite, ported here as Kira
//! source — behavior, never internals.
//!
//! Two things are gated, and they catch different failures. The suite catches a
//! *wrong value*: a case computes something and asserts what it should be. The
//! checksum run catches a *backend divergence*: the same program printing
//! different bytes on two engines is a parity bug even when every case passes,
//! because a case only looks at what it thought to look at.

use std::path::{Path, PathBuf};
use std::process::Command;

/// The harness package, relative to this crate.
fn harness() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests-kik/harness")
}

/// Runs the shipped binary against the harness with this checkout's Foundation.
///
/// Pinned rather than discovered: the harness exercises its own test runner,
/// and an installed toolchain's Foundation is not the test package's source.
fn kira(args: &[&str]) -> std::process::Output {
    let foundation = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../foundation")
        .canonicalize()
        .expect("the checkout's foundation");
    Command::new(env!("CARGO_BIN_EXE_kira"))
        .env("KIRA_FOUNDATION_HOME", foundation)
        .args(args)
        .output()
        .expect("run kira")
}

/// The FFI harness package, relative to this crate.
///
/// A second suite beside the first, and hybrid-only for a reason: every case in
/// it mixes `@Native` and `@Runtime`, which pure vm and pure llvm refuse. What
/// it exercises is the seam itself — a struct returned by value from native
/// code, an enum crossing into a VM closure, an array written through a
/// `borrow mut` parameter and read back on the other side — which no
/// single-engine suite reaches, because on one engine there is no crossing.
fn ffi_harness() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests-kik/ffi-harness")
}

/// Every case in the FFI harness passes on the hybrid engine.
///
/// The tally is asserted whole, as the suite above does, so a file that stops
/// being compiled fails this rather than quietly shrinking the run.
#[test]
fn the_ffi_harness_passes_on_the_hybrid_engine() {
    let path = ffi_harness();
    let path = path.to_str().expect("a utf-8 path");
    let output = kira(&["test", "--backend", "hybrid", path]);
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(
        output.status.success(),
        "the ffi harness did not run: {}\n{stdout}",
        String::from_utf8_lossy(&output.stderr)
    );
    let tally = stdout.lines().last().unwrap_or_default().to_owned();
    assert!(
        tally.contains("0 failed"),
        "the ffi harness reported failures: {tally}"
    );
    assert_eq!(
        tally, "267 passed, 0 failed, 0 skipped, 267 total",
        "the ffi harness tally changed"
    );
}

/// The raw system-call harness package, relative to this crate.
///
/// Linux-only and LLVM-only, and neither is a choice. A `@FFI.Syscall` is
/// refused at compile time on a target that cannot reach the Linux kernel — that
/// refusal is the feature — and the interpreter has no instruction stream of its
/// own to put `svc`/`syscall` in, so the VM and hybrid engines refuse to start
/// such a program by name.
#[cfg(target_os = "linux")]
fn syscall_harness() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests-kik/syscall-harness")
}

/// Every case in the system-call harness passes against a real kernel.
///
/// Gated on the operating system rather than skipped inside the suite: on a
/// machine that is not Linux there is no program to run, because the frontend
/// refuses the declarations. The tally is asserted whole for the same reason the
/// others are — a case that stops being collected fails this rather than quietly
/// shrinking the run, which is exactly what a bodyless declaration in the same
/// file used to cause.
#[test]
#[cfg(target_os = "linux")]
fn the_syscall_harness_passes_against_the_kernel() {
    let path = syscall_harness();
    let path = path.to_str().expect("a utf-8 path");
    let output = kira(&["test", "--backend", "llvm", path]);
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(
        output.status.success(),
        "the syscall harness did not run: {}\n{stdout}",
        String::from_utf8_lossy(&output.stderr)
    );
    let tally = stdout.lines().last().unwrap_or_default().to_owned();
    assert_eq!(
        tally, "18 passed, 0 failed, 0 skipped, 18 total",
        "the syscall harness tally changed"
    );
}

/// The VM refuses a program that calls the kernel, by name, before it starts.
///
/// The refusal names every call the program makes and the flag that does work,
/// because a message saying only "the VM cannot do this" leaves the reader to
/// guess which of the two halves of their command line to change.
#[test]
#[cfg(target_os = "linux")]
fn the_vm_refuses_a_program_that_calls_the_kernel() {
    let path = syscall_harness();
    let path = path.to_str().expect("a utf-8 path");
    let output = kira(&["run", "--backend", "vm", path]);
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(!output.status.success(), "the VM ran it anyway: {stderr}");
    assert!(
        stderr.contains("calls the Linux kernel directly"),
        "unexpected refusal: {stderr}"
    );
    assert!(
        stderr.contains("write"),
        "the calls are not named: {stderr}"
    );
    assert!(
        stderr.contains("--backend llvm"),
        "the working flag is not named: {stderr}"
    );
}

/// The last line of a run, which is the suite's tally.
fn tally(backend: &str) -> String {
    let path = harness();
    let path = path.to_str().expect("a utf-8 path");
    let output = kira(&["test", "--backend", backend, path]);
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(
        output.status.success(),
        "the harness did not run on {backend}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    stdout.lines().last().unwrap_or_default().to_owned()
}

/// Every case that runs, passes — on the VM and on native alike, with the two
/// agreeing on the count.
///
/// The tally is compared as a whole rather than the failure count alone, so a
/// case that stops being *collected* fails this too. A suite that silently
/// shrinks is the failure mode a "zero failures" assertion cannot see.
#[test]
fn the_harness_suite_passes_identically_on_vm_and_native() {
    let vm = tally("vm");
    let llvm = tally("llvm");
    assert_eq!(vm, "1230 passed, 0 failed, 0 skipped, 1230 total");
    assert_eq!(vm, llvm, "the vm and native backends disagree on the suite");
}

/// The checksum run prints the same bytes on both engines.
///
/// This is the half a passing suite cannot prove. Each area reduces to an `Int`
/// derived from everything it computed, so one wrong value anywhere changes a
/// checksum — including values no case thought to assert.
#[test]
fn the_harness_checksums_match_across_backends() {
    let path = harness();
    let path = path.to_str().expect("a utf-8 path");
    let vm = kira(&["run", "--backend", "vm", path]);
    let llvm = kira(&["run", "--backend", "llvm", path]);
    assert!(
        vm.status.success() && llvm.status.success(),
        "the checksum run failed: {} {}",
        String::from_utf8_lossy(&vm.stderr),
        String::from_utf8_lossy(&llvm.stderr)
    );
    let (vm, llvm) = (
        String::from_utf8_lossy(&vm.stdout),
        String::from_utf8_lossy(&llvm.stdout),
    );
    assert!(
        vm.starts_with("kik-harness-begin"),
        "unexpected output: {vm}"
    );
    assert!(
        vm.trim_end().ends_with("kik-harness-end"),
        "truncated: {vm}"
    );
    assert_eq!(vm, llvm, "the vm and native backends print different bytes");
}
