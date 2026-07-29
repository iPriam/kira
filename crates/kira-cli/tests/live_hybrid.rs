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

use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

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

/// Runs `kira live` on `path` with `backend` and returns (stdout, stderr, ok).
fn live(path: &Path, backend: &str) -> (String, String, bool) {
    live_with(path, backend, &[])
}

/// A child that is killed when the test drops it, however the test ends.
///
/// A watched session runs until its own `--quit-after`, so a test that stops
/// reading — because it saw what it came for, or because it failed — must not
/// leave one running. Killing on drop is what turns a stuck session into a
/// failing test rather than a hanging suite.
struct Session(Child);

impl Session {
    /// Ends the session now, rather than at the end of the scope.
    ///
    /// Reading the session's stderr to EOF means waiting for the process to
    /// exit, so it has to be stopped first or the read waits out the ceiling.
    fn stop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Runs a watched `kira live` session, edits the program once the session is
/// actually watching, and reads until `done` says enough has arrived — then
/// stops the session and returns what it printed.
///
/// Both halves wait on an *event*, never on a duration. `done` waits on the
/// reload/relaunch signal because a rebuild — for a hybrid app, an LLVM compile
/// and a link — takes as long as the loaded machine makes it take; the
/// session's `--quit-after` stays only as a generous ceiling so a session that
/// never gets there fails instead of hanging.
///
/// `edit` waits on `live.watch.started` for the same reason. The watcher takes
/// its baseline the instant it starts, and a save that lands before that is
/// folded into the baseline and never seen as a change — so an edit fired on a
/// fixed delay races the initial build, and under a parallel `cargo test` that
/// build can outlast the delay, the change is lost, and the reload never fires.
/// Firing the edit only after the session announces it is watching removes the
/// race outright: the save is guaranteed to land after the baseline.
fn live_until(
    path: &Path,
    backend: &str,
    extra: &[&str],
    edit: impl FnOnce(),
    done: impl Fn(&str) -> bool,
) -> (String, String) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_kira"))
        .arg("live")
        .arg("--backend")
        .arg(backend)
        .args(extra)
        .arg(path)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("kira spawns");

    let stdout_pipe = child.stdout.take().expect("stdout is piped");
    let stderr_pipe = child.stderr.take().expect("stderr is piped");
    let mut session = Session(child);

    // stderr is drained on its own thread: a session that fills the pipe would
    // otherwise block forever on a write nobody is reading.
    let stderr_reader = std::thread::spawn(move || {
        let mut text = String::new();
        let mut pipe = stderr_pipe;
        let _ = pipe.read_to_string(&mut text);
        text
    });

    let mut edit = Some(edit);
    let mut stdout = String::new();
    for line in BufReader::new(stdout_pipe).lines() {
        let Ok(line) = line else {
            break;
        };
        stdout.push_str(&line);
        stdout.push('\n');
        // The instant the session is watching, make the edit — never before, or
        // the save is baked into the baseline and no change is ever reported.
        if stdout.contains("live.watch.started")
            && let Some(edit) = edit.take()
        {
            edit();
        }
        if done(&stdout) {
            break;
        }
    }
    // Stop the session so the ceiling is never actually waited out.
    session.stop();
    let stderr = stderr_reader.join().unwrap_or_default();
    (stdout, stderr)
}

/// Runs `kira live` with extra arguments.
fn live_with(path: &Path, backend: &str, extra: &[&str]) -> (String, String, bool) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_kira"))
        .arg("live")
        .arg("--backend")
        .arg(backend)
        .args(extra)
        .arg(path)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("kira spawns");

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
    let status = child.wait().expect("kira exits");
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

/// A `@Runtime`-only edit to a hybrid app hot-patches, keeping the loaded
/// native library.
///
/// This is the case the whole tier decision exists for, and it is the one that
/// was shipped on a manual run: every other reload test builds VM bundles, where
/// there is no library and "the library survives" is vacuously true. Here there
/// is a real `dlopen`ed dylib, and the edit must not disturb it.
///
/// The proof is `mode=hotpatch` plus the absence of `live.runner.relaunched`:
/// one process, one connection, and the native half never reloaded.
#[test]
fn a_runtime_only_edit_to_a_hybrid_app_hot_patches() {
    let scratch = Scratch::new("hybrid-reload");
    let program = scratch.program(HYBRID_PROGRAM);

    // Edit only the @Runtime half, mid-session, the instant the session is
    // watching. The native half is untouched, so it must rebuild byte-identical
    // and the swap must be allowed.
    let edited = program.clone();
    let edit = move || {
        std::fs::write(
            &edited,
            HYBRID_PROGRAM.replace("return double(n) + 1", "return double(n) + 5000"),
        )
        .expect("edit the program");
    };

    // Read until both halves of the evidence have arrived: the swapped-in code
    // has printed, and the session has called the reload done. The ceiling only
    // bounds a session that never gets there.
    let (stdout, stderr) = live_until(
        &program,
        "hybrid",
        &["--watch", "--quit-after", "180s"],
        edit,
        |seen| seen.contains("\n5020\n") && seen.contains("live.reload.completed"),
    );

    assert!(
        stdout.contains("live.reload.completed mode=hotpatch"),
        "a @Runtime-only edit beside an unchanged native library must hot patch.\n\
         stdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        !stdout.contains("live.runner.relaunched"),
        "the runner was relaunched, so the native library did not survive.\n\
         stdout: {stdout}"
    );
    // 5020 = double(10) + 5000, where `double` is the @Native half: the
    // swapped-in bytecode calling into the library that was already loaded. It
    // proves both that the new code ran and that the library it calls still
    // worked across the swap — a re-mapped or unloaded library would not have
    // answered.
    assert!(
        stdout.contains("\n5020\n"),
        "the swapped-in code must call into the surviving native library.\n\
         stdout: {stdout}"
    );
}

/// A `@Native` edit to a hybrid app relaunches, and says why.
///
/// The other half of the tier decision, over a real dylib: the library's bytes
/// moved, so the running process has stale code mapped and cannot be patched.
#[test]
fn a_native_edit_to_a_hybrid_app_relaunches() {
    let scratch = Scratch::new("hybrid-relaunch");
    let program = scratch.program(HYBRID_PROGRAM);

    let edited = program.clone();
    let edit = move || {
        std::fs::write(
            &edited,
            HYBRID_PROGRAM.replace("return n * 2", "return n * 3"),
        )
        .expect("edit the program");
    };

    // A relaunch ends at `live.runner.relaunched`, not at
    // `live.reload.completed`: only the hot-patch tier reports a completion,
    // because only there is there a running process that took the swap. So the
    // terminal event to wait for is the relaunch itself.
    let (stdout, stderr) = live_until(
        &program,
        "hybrid",
        &["--watch", "--quit-after", "180s"],
        edit,
        |seen| seen.contains("\n126\n") && seen.contains("live.runner.relaunched"),
    );

    assert!(
        stdout.contains("mode=relaunch"),
        "a native edit must relaunch.\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        stdout.contains("live.runner.relaunched"),
        "the runner must actually be replaced.\nstdout: {stdout}"
    );
    // The reason reaches the user rather than a bare restart.
    assert!(
        stdout.contains("the native library") && stdout.contains("changed"),
        "the relaunch must say what changed.\nstdout: {stdout}"
    );
    // 126 = 42 * 3: the relaunched runner really did load the new native code.
    assert!(
        stdout.contains("\n126\n"),
        "the relaunched app must run the new native code.\nstdout: {stdout}"
    );
}
