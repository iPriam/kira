//! `kira live` on a package, driven the way a user drives it: from inside the
//! package directory, with no path typed at all.
//!
//! Two claims, both of which were false before: a bare `kira live` is the
//! package you are standing in — the default `run`, `build`, and `check` already
//! took — and a watched session on a package watches the package, so a save to a
//! module the entry imports reloads the running app.
//!
//! VM bundles throughout, so this runs everywhere the CLI's test suite runs.
//! What a native bundle carries over the same socket is `live_hybrid`'s subject.

use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};

/// An app package that removes itself, entry and modules alike.
struct Package(PathBuf);

impl Package {
    /// Writes an app package whose entry is `main` and whose `app/` also holds
    /// `modules`, named without the `.kira` suffix.
    fn new(tag: &str, main: &str, modules: &[(&str, &str)]) -> Package {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "kira-live-package-{}-{tag}-{unique}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("app")).expect("package tree");
        std::fs::write(
            root.join("package.kira"),
            "Package liveapp {\n    let version = \"0.1.0\"\n    let kind = .App\n}\n",
        )
        .expect("write package.kira");
        let package = Package(root);
        package.write("app/main.kira", main);
        for (name, source) in modules {
            package.write(&format!("app/{name}.kira"), source);
        }
        package
    }

    fn path(&self) -> &Path {
        &self.0
    }

    /// Writes `text` to `relative` inside the package.
    fn write(&self, relative: &str, text: &str) -> PathBuf {
        let path = self.0.join(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("package directory");
        }
        std::fs::write(&path, text).expect("write package source");
        path
    }
}

impl Drop for Package {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A child that is killed when the test drops it, however the test ends.
///
/// A watched session runs until its own `--quit-after`, so a test that stops
/// reading must not leave one running: killing on drop turns a stuck session
/// into a failing test rather than a hanging suite.
struct Session(Child);

impl Session {
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

/// Runs `kira live` *inside* `directory` with `extra` and nothing else — no
/// path, which is the whole point — and returns (stdout, stderr, ok).
///
/// `--no-watch` because this reads the session's output to end of file, which
/// only arrives when the session ends: a watched session is one that does not,
/// and asking for the one-shot in words is what says so.
fn live_in(directory: &Path, extra: &[&str]) -> (String, String, bool) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_kira"))
        .arg("live")
        .arg("--no-watch")
        .args(extra)
        .current_dir(directory)
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

/// Runs a watched bare `kira live` inside `directory`, edits once the session
/// says it is watching, and reads until `done` has what it came for.
///
/// Every wait is on an event rather than a duration. The watcher takes its
/// baseline the instant it starts, so an edit fired on a delay races the initial
/// build and is folded into the baseline under a loaded machine; firing it on
/// `live.watch.started` removes the race. `--quit-after` stays a ceiling only.
fn live_until(
    directory: &Path,
    extra: &[&str],
    edit: impl FnOnce(),
    done: impl Fn(&str) -> bool,
) -> (String, String) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_kira"))
        .arg("live")
        .args(extra)
        .current_dir(directory)
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
        if stdout.contains("live.watch.started")
            && let Some(edit) = edit.take()
        {
            edit();
        }
        if done(&stdout) {
            break;
        }
    }
    session.stop();
    let stderr = stderr_reader.join().unwrap_or_default();
    (stdout, stderr)
}

/// An entry that gets its answer from a module beside it, so the module is what
/// a reload has to notice.
const ENTRY: &str = "import numbers\n\
     @Main function main() { print(answer()) return }\n";

/// The module the entry imports.
const MODULE: &str = "function answer() -> Int { return 7 }\n";

/// `kira live` with no path at all, run from inside a package.
///
/// The invocation a user types in their app directory. It used to be a usage
/// error — `expected a path to a .kira file` — while `kira run` in the same
/// directory ran the package.
#[test]
fn a_bare_live_session_runs_the_package_you_are_standing_in() {
    let package = Package::new("bare", ENTRY, &[("numbers", MODULE)]);

    let (stdout, stderr, ok) = live_in(package.path(), &[]);

    assert!(ok, "a bare live session must exit 0.\nstderr: {stderr}");
    assert!(
        stdout.contains("\n7\n") || stdout.starts_with("7\n"),
        "the package's program must run.\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        stdout.contains("live.session.ready"),
        "the session must reach ready.\nstdout: {stdout}\nstderr: {stderr}"
    );
}

/// The flags a bare invocation carries still parse, and still leave the path
/// defaulted: `kira live --backend vm` is a session on the package here.
#[test]
fn a_bare_live_session_takes_flags() {
    let package = Package::new("flags", ENTRY, &[("numbers", MODULE)]);

    let (stdout, stderr, ok) = live_in(package.path(), &["--backend", "vm"]);

    assert!(ok, "stderr: {stderr}");
    assert!(
        stdout.contains("live.bundle.built payloads=1"),
        "a vm bundle is one payload.\nstdout: {stdout}\nstderr: {stderr}"
    );
}

/// A directory that is not a package is refused by name, rather than by a usage
/// error about a missing argument.
///
/// This is what the default buys: the path goes through the same package
/// discovery an explicit one does, so the diagnostic names `package.kira`.
#[test]
fn a_bare_live_session_outside_a_package_names_the_manifest() {
    let empty = std::env::temp_dir().join(format!(
        "kira-live-package-{}-not-a-package",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&empty);
    std::fs::create_dir_all(&empty).expect("scratch dir");

    let (_, stderr, ok) = live_in(&empty, &[]);
    let _ = std::fs::remove_dir_all(&empty);

    assert!(!ok, "a directory with no manifest is not a session");
    assert!(
        stderr.contains("package.kira"),
        "the refusal must name what is missing.\nstderr: {stderr}"
    );
}

/// Editing a module the entry imports reloads the session.
///
/// The watch set is the package, not the entry file: a session that watched only
/// `app/main.kira` would sit there while every other file in the package changed
/// underneath it.
#[test]
fn a_watched_package_session_reloads_on_a_save_to_a_module() {
    let package = Package::new("reload", ENTRY, &[("numbers", MODULE)]);
    let module = package.path().join("app/numbers.kira");

    let (stdout, stderr) = live_until(
        package.path(),
        // The ceiling is generous: the wait below is on the reload event, and
        // this only stops a session that never gets there from hanging.
        &["--watch", "--quit-after", "60s"],
        move || {
            std::fs::write(&module, "function answer() -> Int { return 9 }\n")
                .expect("edit the module");
        },
        // Both, because neither implies the other has arrived: the app's print
        // and the server's milestone come from two processes over one pipe, in
        // either order. Stopping on the first closes the connection before the
        // second, which is a failure about timing rather than about reloading.
        |stdout| {
            stdout.contains("\n9\n")
                && (stdout.contains("live.reload.applied")
                    || stdout.contains("live.runner.relaunched"))
        },
    );

    assert!(
        stdout.contains("live.watch.started"),
        "the session must start watching.\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        stdout.contains("live.reload.applied") || stdout.contains("live.runner.relaunched"),
        "a save to an imported module must reach the running app.\n\
         stdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        stdout.contains("\n9\n"),
        "the reloaded app must print the edited module's answer.\n\
         stdout: {stdout}\nstderr: {stderr}"
    );
}
