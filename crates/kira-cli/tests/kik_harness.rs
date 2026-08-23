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
        tally, "270 passed, 0 failed, 0 skipped, 270 total",
        "the ffi harness tally changed"
    );
}

/// The raw system-call harness package, relative to this crate.
///
/// Linux-only, and that is not a choice: a `@FFI.Syscall` is refused at compile
/// time on a target that cannot reach the Linux kernel — that refusal is the
/// feature — so on another operating system there is no program here to run.
///
/// This package holds the calls that only an emitted instruction can make;
/// [`syscall_parity_harness`] holds the ones a host can make for an interpreted
/// program. The split is what the VM's refusal forces, and both READMEs say so.
#[cfg(target_os = "linux")]
fn syscall_harness() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests-kik/syscall-harness")
}

/// Every case in the system-call harness passes against a real kernel, on both
/// engines that emit the instruction.
///
/// Both are asserted rather than one, because they reach the kernel by different
/// routes and only running both proves the second. On `llvm` the call site is
/// machine code in the program. On `hybrid` the bodies holding the calls are the
/// native half and a `Test` reaches them across the bridge, which is the same
/// crossing the FFI harness exercises — so a change that broke the native half's
/// lowering while leaving a whole-program build working would fail here.
///
/// Gated on the operating system rather than skipped inside the suite, and the
/// tally asserted whole for the same reason the others are: a case that stops
/// being collected fails this rather than quietly shrinking the run, which is
/// exactly what a bodyless declaration in the same file used to cause.
#[test]
#[cfg(target_os = "linux")]
fn the_syscall_harness_passes_against_the_kernel() {
    for backend in ["llvm", "hybrid"] {
        let path = syscall_harness();
        let path = path.to_str().expect("a utf-8 path");
        let output = kira(&["test", "--backend", backend, path]);
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        assert!(
            output.status.success(),
            "the syscall harness did not run on {backend}: {}\n{stdout}",
            String::from_utf8_lossy(&output.stderr)
        );
        let tally = stdout.lines().last().unwrap_or_default().to_owned();
        assert_eq!(
            tally, "19 passed, 0 failed, 0 skipped, 19 total",
            "the syscall harness tally changed on {backend}"
        );
    }
}

/// The servable-system-call package, relative to this crate.
///
/// The other half of the suite above, and Linux-only for the same reason. It
/// declares only the four calls an interpreter can serve, which is what lets one
/// source run on all three engines.
#[cfg(target_os = "linux")]
fn syscall_parity_harness() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests-kik/syscall-parity-harness")
}

/// Every case in the servable-system-call package passes on all three engines.
///
/// The leg the suite above cannot have. `syscall-harness` names `mount`,
/// `execve`, `wait4` and `umount2`, and the VM refuses a program that names one
/// before it starts — so it can never run there, and its lowering had no
/// interpreter to be checked against. This package makes only the calls a host
/// can make on a program's behalf, so the interpreter and the emitted
/// instruction can be asked the same question.
///
/// The tally is asserted whole, as every harness here does: a case that stops
/// being collected fails this rather than quietly shrinking the run.
#[test]
#[cfg(target_os = "linux")]
fn the_servable_syscall_harness_passes_on_every_engine() {
    for backend in ["vm", "llvm", "hybrid"] {
        let path = syscall_parity_harness();
        let path = path.to_str().expect("a utf-8 path");
        let output = kira(&["test", "--backend", backend, path]);
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        assert!(
            output.status.success(),
            "the servable syscall harness did not run on {backend}: {}\n{stdout}",
            String::from_utf8_lossy(&output.stderr)
        );
        let tally = stdout.lines().last().unwrap_or_default().to_owned();
        assert_eq!(
            tally, "9 passed, 0 failed, 0 skipped, 9 total",
            "the servable syscall harness tally changed on {backend}"
        );
    }
}

/// The same kernel answers print the same bytes on all three engines.
///
/// The half a passing suite cannot prove, for the one feature that had no such
/// check at all. Each printed number is derived from a real `-errno` the kernel
/// put in a register, so a host that decoded an answer the emitted call leaves
/// raw — or sign-extended a narrow descriptor differently — changes a line here
/// even when every case still passes.
#[test]
#[cfg(target_os = "linux")]
fn the_servable_syscall_harness_prints_the_same_bytes_on_every_engine() {
    let path = syscall_parity_harness();
    let path = path.to_str().expect("a utf-8 path");
    let runs: Vec<(&str, String)> = ["vm", "llvm", "hybrid"]
        .into_iter()
        .map(|backend| {
            let output = kira(&["run", "--backend", backend, path]);
            assert!(
                output.status.success(),
                "the servable syscall run failed on {backend}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            (
                backend,
                String::from_utf8_lossy(&output.stdout).into_owned(),
            )
        })
        .collect();
    assert!(
        runs[0].1.contains("kik-syscall-parity-end"),
        "the run did not finish: {}",
        runs[0].1
    );
    for (backend, stdout) in &runs[1..] {
        assert_eq!(
            &runs[0].1, stdout,
            "the vm and {backend} backends disagree on the kernel's answers"
        );
    }
}

/// The VM refuses a program naming a call no interpreter can serve, by name and
/// with the reason, before it starts.
///
/// Two halves, and both matter. It refuses, because `syscall-harness` names
/// seven calls that act on the interpreter's process or on the machine — and it
/// names each of them with what it would have done, because "the VM cannot do
/// this" leaves the reader to guess whether the fix is the program or the
/// command line. It does *not* name `write`, `read` or `ppoll`: those are served
/// now, and a refusal listing them would send an author to change a call that
/// works.
///
/// `sync` is asserted on the refused side rather than the served one, which is
/// the assertion this test exists to have. It takes no descriptor — it flushes
/// every filesystem on the machine — so serving it under the interpreter acts on
/// the developer's box, and on a 9p mount it does so uninterruptibly.
#[test]
#[cfg(target_os = "linux")]
fn the_vm_refuses_only_the_calls_no_interpreter_can_serve() {
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
        stderr.contains("the process is the interpreter rather than the program"),
        "the refusal does not say why: {stderr}"
    );
    for call in [
        "sync",
        "mount",
        "umount2",
        "reboot",
        "execve",
        "wait4",
        "exit_group",
    ] {
        assert!(
            stderr.contains(&format!("`{call}`")),
            "`{call}` is not named: {stderr}"
        );
    }
    for served in ["`write`", "`read`", "`ppoll`"] {
        assert!(
            !stderr.contains(served),
            "{served} is served on the VM and must not be refused: {stderr}"
        );
    }
    assert!(
        stderr.contains("--backend llvm") && stderr.contains("--backend hybrid"),
        "the engines that do work are not both named: {stderr}"
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
    assert_eq!(vm, "1293 passed, 0 failed, 0 skipped, 1293 total");
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
