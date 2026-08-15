//! A live session over native code, end to end.
//!
//! These tests use the managed LLVM toolchain because they build real native
//! libraries, send them over the live protocol, and load them in a runner.
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

/// Runs `kira live` with extra arguments, one shot.
///
/// `--no-watch` in words: this reads the session's output to end of file, and
/// that only arrives when the session ends. A watched session is one that does
/// not end on its own, which is the opposite of what is being read for.
fn live_with(path: &Path, backend: &str, extra: &[&str]) -> (String, String, bool) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_kira"))
        .arg("live")
        .arg("--no-watch")
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

const HYBRID_FIXTURE: &str = include_str!("fixtures/live/hybrid_native.kira");
const ORDINARY_FIXTURE: &str = include_str!("fixtures/live/ordinary.kira");

/// The program's own lines, without live-session milestones.
fn app_output(stdout: &str) -> Vec<String> {
    stdout
        .lines()
        .filter(|line| !line.starts_with("live."))
        .map(str::to_owned)
        .collect()
}

/// VM, LLVM, and hybrid live sessions run an ordinary checked-in program
/// identically.
#[test]
fn all_live_backends_agree_on_a_checked_in_runtime_program() {
    let scratch = Scratch::new("ordinary");
    let program = scratch.program(ORDINARY_FIXTURE);

    let (vm_stdout, vm_stderr, vm_ok) = live(&program, "vm");
    let (llvm_stdout, llvm_stderr, llvm_ok) = live(&program, "llvm");
    let (hybrid_stdout, hybrid_stderr, hybrid_ok) = live(&program, "hybrid");

    assert!(vm_ok, "the vm session failed.\nstderr: {vm_stderr}");
    assert!(llvm_ok, "the llvm session failed.\nstderr: {llvm_stderr}");
    assert!(
        hybrid_ok,
        "the hybrid session failed.\nstderr: {hybrid_stderr}"
    );
    assert_eq!(app_output(&vm_stdout), ["Harmony Browser", "3", "6"]);
    assert!(
        llvm_stdout
            .lines()
            .any(|line| line.starts_with("live.bundle.built ")),
        "an ordinary LLVM live bundle must be built.\nstdout: {llvm_stdout}"
    );
    assert_eq!(app_output(&llvm_stdout), app_output(&vm_stdout));
    assert_eq!(app_output(&hybrid_stdout), app_output(&vm_stdout));
}

/// The native half runs, across a real socket, in a real runner process.
#[test]
fn a_hybrid_live_session_runs_the_native_half() {
    let scratch = Scratch::new("native");
    let program = scratch.program(HYBRID_FIXTURE);

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

/// LLVM live runs the same native-oriented program as one whole native image
/// inside the runner process.
#[test]
fn an_llvm_live_session_runs_the_whole_program_natively() {
    let scratch = Scratch::new("llvm-native");
    let program = scratch.program(HYBRID_FIXTURE);

    let (stdout, stderr, ok) = live(&program, "llvm");

    assert!(ok, "an llvm live session must exit 0.\nstderr: {stderr}");
    assert_eq!(
        app_output(&stdout),
        vec!["84".to_owned(), "21".to_owned()],
        "the LLVM entry must run in the desktop runner process.\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        stdout.contains("live.session.ready"),
        "the session must reach ready.\nstdout: {stdout}"
    );
}

/// A hybrid bundle carries its manifest, bytecode half, and native library.
/// The event proves the bundle was built; readiness and output prove both halves
/// reached the runner.
#[test]
fn a_hybrid_bundle_carries_both_halves() {
    let scratch = Scratch::new("payloads");
    let program = scratch.program(HYBRID_FIXTURE);

    let (stdout, stderr, ok) = live(&program, "hybrid");

    assert!(ok, "stderr: {stderr}");
    assert!(
        stdout
            .lines()
            .any(|line| line.starts_with("live.bundle.built ")),
        "a hybrid bundle must be built.\nstdout: {stdout}"
    );
}

/// Every live backend runs the same program to the same answers over a live session.
///
/// The point of the dual-mode promise is that where code runs does not change
/// what it does. A live session is a place that could break it because each
/// backend stages a different entry shape and resolves it in a runner's cache,
/// so the backends are compared rather than assumed to agree.
#[test]
fn all_live_backends_agree_over_a_live_session() {
    let scratch = Scratch::new("parity");
    let program = scratch.program(HYBRID_FIXTURE);

    let (vm_stdout, vm_stderr, vm_ok) = live(&program, "vm");
    let (llvm_stdout, llvm_stderr, llvm_ok) = live(&program, "llvm");
    let (hybrid_stdout, hybrid_stderr, hybrid_ok) = live(&program, "hybrid");

    assert!(vm_ok, "the vm session failed.\nstderr: {vm_stderr}");
    assert!(llvm_ok, "the llvm session failed.\nstderr: {llvm_stderr}");
    assert!(
        hybrid_ok,
        "the hybrid session failed.\nstderr: {hybrid_stderr}"
    );

    assert_eq!(
        app_output(&vm_stdout),
        app_output(&llvm_stdout),
        "the whole native program must preserve the VM answers"
    );
    assert_eq!(
        app_output(&vm_stdout),
        app_output(&hybrid_stdout),
        "the same program must print the same thing on every backend"
    );
    assert_eq!(
        app_output(&vm_stdout),
        vec!["84".to_owned(), "21".to_owned()]
    );
}

/// A `@Runtime`-only edit to a hybrid app relaunches until bytecode
/// compatibility evidence exists.
///
/// This is a true hybrid reload: the bundle has a real `dlopen`ed dylib, and a
/// bytecode-only edit must relaunch rather than swap code without live-value
/// compatibility evidence.
///
/// The proof is `mode=relaunch` plus the new native-backed result after the
/// runner has loaded the rebuilt bundle.
#[test]
fn a_runtime_only_edit_to_a_hybrid_app_relaunches() {
    let scratch = Scratch::new("hybrid-reload");
    let program = scratch.program(HYBRID_FIXTURE);

    // Edit only the @Runtime half, mid-session, the instant the session is
    // watching. The native half is untouched, but the changed bytecode has no
    // live-value compatibility evidence in KLB1, so the session must relaunch.
    let edited = program.clone();
    let edit = move || {
        std::fs::write(
            &edited,
            HYBRID_FIXTURE.replace("return double(n) + 1", "return double(n) + 5000"),
        )
        .expect("edit the program");
    };

    // Read until both halves of the evidence have arrived: the relaunched code
    // has printed, and the runner has announced the relaunch. The ceiling only
    // bounds a session that never gets there.
    let (stdout, stderr) = live_until(
        &program,
        "hybrid",
        &["--watch", "--quit-after", "180s"],
        edit,
        |seen| seen.contains("\n5020\n") && seen.contains("live.runner.relaunched"),
    );

    assert!(
        stdout.contains("mode=relaunch"),
        "a changed hybrid bytecode module must relaunch without compatibility evidence.\n\
         stdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        stdout.contains("live.runner.relaunched"),
        "the hybrid runner must be relaunched for changed bytecode.\n\
         stdout: {stdout}"
    );
    // 5020 = double(10) + 5000, where `double` is the @Native half. It proves
    // the relaunched hybrid runner loaded and called the native library.
    assert!(
        stdout.contains("\n5020\n"),
        "the relaunched code must call into the native library.\n\
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
    let program = scratch.program(HYBRID_FIXTURE);

    let edited = program.clone();
    let edit = move || {
        std::fs::write(
            &edited,
            HYBRID_FIXTURE.replace("return n * 2", "return n * 3"),
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
