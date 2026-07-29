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
/// Pinned rather than discovered: the harness exercises the runner Foundation
/// generates, and an installed toolchain's copy is whatever was last installed
/// rather than what this tree says.
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
    assert!(
        vm.contains("0 failed"),
        "the harness reported failures on the vm: {vm}"
    );
    assert_eq!(vm, llvm, "the vm and native backends disagree on the suite");
    // A guard on the port itself: the areas ported so far carry over a thousand
    // cases, so a tally an order of magnitude smaller means files stopped being
    // compiled rather than that the suite got faster.
    let passed: usize = vm
        .split_whitespace()
        .next()
        .and_then(|count| count.parse().ok())
        .unwrap_or_default();
    assert!(passed > 1000, "only {passed} cases ran: {vm}");
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
