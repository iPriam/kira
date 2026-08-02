//! What the runner's host provides a running bundle.
//!
//! A live session runs the program in the *runner's* process, against a host
//! that runner builds — not the one `kira run` builds. Anything the CLI's host
//! stack provides and the runner's does not is a capability that works
//! everywhere except live, which is the worst place for a gap to hide: the
//! program compiles, the bundle links, and the trap arrives at the entrypoint.
//!
//! VM backend, so this runs wherever the CLI's suite runs.

use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Stdio};
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

/// Runs one unwatched `kira live --backend vm` session and returns
/// (stdout, stderr, ok).
fn live(scratch: &Scratch) -> (String, String, bool) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_kira"))
        .args(["live", "--backend", "vm"])
        .arg(scratch.program())
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

/// A program whose answer only a host with callback-state storage can produce.
const NATIVE_STATE_PROGRAM: &str = r#"
struct State { var count: Int }

@Main
function main() {
    var original = State { count: 3 }
    var state = nativeState(original)
    original.count = 9
    var recovered = nativeRecover<State>(nativeUserData(state))
    print(original.count)
    print(recovered.count)
    nativeStateFree(state)
}
"#;

/// Callback state works over a live session.
///
/// The storage is the host's, not the VM's, so a runner that hands the program
/// a bare stdout host traps here with "this host does not provide native
/// callback-state storage" — at the entrypoint, after a bundle that built,
/// linked, and reported ready. Every UI app boxes state for a native callback
/// on its first frame, so that trap is the difference between live working and
/// live being unusable for the programs it exists to serve.
#[test]
fn the_runner_provides_native_callback_state() {
    let scratch = Scratch::new("native-state", NATIVE_STATE_PROGRAM);

    let (stdout, stderr, ok) = live(&scratch);

    assert!(ok, "the session must exit 0.\nstderr: {stderr}");
    assert!(
        stdout.contains("\n9\n3\n"),
        "the program's own output must be the VM's.\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        !stderr.contains("callback-state"),
        "no host-capability trap belongs here.\nstderr: {stderr}"
    );
}
