//! A live session over the *native* half, end to end.
//!
//! Gated on `llvm` because building a hybrid bundle needs the backend; CI has no
//! LLVM, so this is skipped there and the VM-side live tests are what run. That
//! is exactly why this file exists: without it, every live test in the workspace
//! builds VM bundles, and the claim that a live session carries native code
//! would rest on a manual run someone did once.
//!
//! What it proves is not that the session reports milestones — the VM tests
//! cover the protocol. It is that a native library survives the trip: compiled
//! here, hashed into a bundle, sent over a socket, staged on the far side, and
//! `dlopen`ed by a runner that then calls into it. The app's output is the
//! evidence, and the value it prints is one only the native half computes.
#![cfg(feature = "llvm")]

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// A scratch directory that removes itself, holding a program and its build.
struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Scratch {
        let path =
            std::env::temp_dir().join(format!("kira-live-hybrid-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("scratch dir");
        Scratch(path)
    }

    /// Writes `source` as a `.kira` file and returns its path.
    fn program(&self, source: &str) -> PathBuf {
        let path = self.0.join("app.kira");
        std::fs::write(&path, source).expect("write program");
        path
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Runs `kirac live` on `path` with `backend` and returns (stdout, stderr, ok).
fn live(path: &Path, backend: &str) -> (String, String, bool) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_kirac"))
        .arg("live")
        .arg("--backend")
        .arg(backend)
        .arg(path)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("kirac spawns");

    let mut stdout = String::new();
    child
        .stdout
        .take()
        .expect("stdout is piped")
        .read_to_string(&mut stdout)
        .expect("read stdout");
    let mut stderr = String::new();
    child
        .stderr
        .take()
        .expect("stderr is piped")
        .read_to_string(&mut stderr)
        .expect("read stderr");
    let status = child.wait().expect("kirac exits");
    (stdout, stderr, status.success())
}

/// A program whose answer comes out of the native half.
///
/// `double` is `@Native`, so in a hybrid build its body is compiled machine code
/// in the bundle's library. Printing `84` means the runner `dlopen`ed that
/// library and called into it: the VM half has no body for `double` to run.
const HYBRID_PROGRAM: &str = r#"
@Native
function double(n: Int) -> Int {
    return n * 2
}

@Runtime
function describe(n: Int) -> Int {
    return double(n) + 1
}

@Main
function main() {
    print(double(42))
    print(describe(10))
    return
}
"#;

/// The native half runs, across a real socket, in a real runner process.
#[test]
fn a_hybrid_live_session_runs_the_native_half() {
    let scratch = Scratch::new("native");
    let program = scratch.program(HYBRID_PROGRAM);

    let (stdout, stderr, ok) = live(&program, "hybrid");

    assert!(ok, "a hybrid live session must exit 0.\nstderr: {stderr}");
    assert!(
        stdout.contains("\n84\n") || stdout.starts_with("84\n"),
        "the native half's answer must appear.\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        stdout.contains("\n21\n"),
        "a runtime function calling into the native half must work.\n\
         stdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        stdout.contains("live.session.ready"),
        "the session must reach ready.\nstdout: {stdout}"
    );
}

/// A hybrid bundle carries three payloads: the manifest, the bytecode half, and
/// the native library. If this ever reads one, the session went VM-only without
/// saying so, and the native claim quietly stopped being true.
#[test]
fn a_hybrid_bundle_carries_both_halves() {
    let scratch = Scratch::new("payloads");
    let program = scratch.program(HYBRID_PROGRAM);

    let (stdout, stderr, ok) = live(&program, "hybrid");

    assert!(ok, "stderr: {stderr}");
    assert!(
        stdout.contains("live.bundle.built payloads=3"),
        "a hybrid bundle is a manifest, a bytecode half, and a native library.\n\
         stdout: {stdout}"
    );
}

/// Both backends run the same program to the same answers over a live session.
///
/// The point of the dual-mode promise is that where code runs does not change
/// what it does. A live session is a place that could break it — the hybrid path
/// stages three payloads and resolves a manifest's siblings in a runner's cache,
/// none of which the VM path does — so the two are compared rather than assumed
/// to agree.
#[test]
fn both_backends_agree_over_a_live_session() {
    let scratch = Scratch::new("parity");
    let program = scratch.program(HYBRID_PROGRAM);

    let (vm_stdout, vm_stderr, vm_ok) = live(&program, "vm");
    let (hybrid_stdout, hybrid_stderr, hybrid_ok) = live(&program, "hybrid");

    assert!(vm_ok, "the vm session failed.\nstderr: {vm_stderr}");
    assert!(
        hybrid_ok,
        "the hybrid session failed.\nstderr: {hybrid_stderr}"
    );

    // Only the app's own lines: the session events differ by payload count and
    // by the port the OS handed out, and neither is the program's behavior.
    let app_output = |stdout: &str| -> Vec<String> {
        stdout
            .lines()
            .filter(|line| !line.starts_with("live."))
            .map(str::to_owned)
            .collect()
    };

    assert_eq!(
        app_output(&vm_stdout),
        app_output(&hybrid_stdout),
        "the same program must print the same thing on both halves"
    );
    assert_eq!(
        app_output(&vm_stdout),
        vec!["84".to_owned(), "21".to_owned()]
    );
}
