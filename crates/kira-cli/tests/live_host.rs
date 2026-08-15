//! What the runner's host provides a running bundle.
//!
//! A live session runs the program in the *runner's* process, against a host
//! that runner builds — not the one `kira run` builds. Anything the CLI's host
//! stack provides and the runner's does not is a capability that works
//! everywhere except live, which is the worst place for a gap to hide: the
//! program compiles, the bundle links, and the trap arrives at the entrypoint.
//!
//! The test runs VM, LLVM, and hybrid bundles through the desktop runner.

use std::io::Read;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};

/// A scratch program that removes itself and its build directory.
struct Scratch(PathBuf);

impl Scratch {
    /// Writes `source` as `app.kira` under a directory of its own.
    fn new(tag: &str, source: &str) -> Scratch {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "kira-live-host-{}-{tag}-{unique}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("scratch dir");
        std::fs::write(root.join("app.kira"), source).expect("write program");
        Scratch(root)
    }

    fn program(&self) -> PathBuf {
        self.0.join("app.kira")
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A spawned `kira` process that is killed when it goes out of scope.
///
/// An unwatched session ends on its own, but only when nothing goes wrong: a
/// panic between the spawn and the wait leaves the process running, and `kira
/// live` supervises a runner of its own, so the survivor holds the inherited
/// pipe and the whole suite reads as leaking. Killing on drop turns either into
/// a failing test rather than a hanging one.
struct Session(Child);

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Runs one unwatched `kira live` session on `backend` and returns
/// (stdout, stderr, ok).
///
/// `--no-watch` in words: the output is read to end of file, which only arrives
/// when the session ends, and a watched session does not end on its own.
fn live(scratch: &Scratch, backend: &str) -> (String, String, bool) {
    let mut session = Session(
        Command::new(env!("CARGO_BIN_EXE_kira"))
            .args(["live", "--no-watch", "--backend", backend])
            .arg(scratch.program())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("kira spawns"),
    );
    let child = &mut session.0;

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

/// The program's own lines, without live-session milestones.
fn app_output(stdout: &str) -> Vec<String> {
    stdout
        .lines()
        .filter(|line| !line.starts_with("live.") && !line.starts_with("@kira"))
        .map(str::to_owned)
        .collect()
}

/// A user-defined callback-state value completes its lifecycle over live.
///
/// The fixture creates a user-defined value, recovers and reads it, writes
/// through the recovered view, reads the write back, and frees the state.
#[test]
fn all_live_backends_provide_native_callback_state() {
    let scratch = Scratch::new(
        "native-state",
        include_str!("fixtures/live/native_state.kira"),
    );

    let (vm_stdout, vm_stderr, vm_ok) = live(&scratch, "vm");
    let (llvm_stdout, llvm_stderr, llvm_ok) = live(&scratch, "llvm");
    let (hybrid_stdout, hybrid_stderr, hybrid_ok) = live(&scratch, "hybrid");

    assert!(vm_ok, "the vm session failed.\nstderr: {vm_stderr}");
    assert!(llvm_ok, "the llvm session failed.\nstderr: {llvm_stderr}");
    assert_eq!(
        app_output(&vm_stdout),
        vec!["Harmony Browser", "1", "3"],
        "the program's own output must be the VM's.\nstdout: {vm_stdout}\nstderr: {vm_stderr}"
    );
    assert!(
        hybrid_ok,
        "the hybrid session failed.\nstderr: {hybrid_stderr}"
    );
    assert!(
        !vm_stderr.contains("callback-state")
            && !llvm_stderr.contains("callback-state")
            && !hybrid_stderr.contains("callback-state"),
        "no host-capability trap belongs here.\nvm stderr: {vm_stderr}\nllvm stderr: {llvm_stderr}\nhybrid stderr: {hybrid_stderr}"
    );
    assert_eq!(app_output(&llvm_stdout), app_output(&vm_stdout));
    assert_eq!(app_output(&hybrid_stdout), app_output(&vm_stdout));
}
