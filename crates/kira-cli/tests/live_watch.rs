//! Bounded end-to-end proofs for the live filesystem-to-runner path.

use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

const SESSION_TIMEOUT: Duration = Duration::from_secs(45);

/// A temporary application tree that removes itself after the test.
struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Scratch {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "kira-live-watch-{}-{tag}-{unique}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("scratch root");
        Scratch(root)
    }

    fn write(&self, relative: &str, contents: &str) -> PathBuf {
        let path = self.0.join(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("scratch parent");
        }
        std::fs::write(&path, contents).expect("scratch file");
        path
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A live CLI process that cannot outlive the test which owns it.
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

/// Runs a watched session until `done` has observed the complete causal chain.
///
/// The stdout reader is a thread so the test can enforce its own deadline while
/// still receiving line-level live events. The child also has `--quit-after` as
/// a second ceiling for a failure that prevents the reader from progressing.
fn live_until(
    path: &Path,
    edit: impl FnOnce(),
    done: impl Fn(&[String]) -> bool,
) -> (Vec<String>, String) {
    let mut session = Session(
        Command::new(env!("CARGO_BIN_EXE_kira"))
            .args(["live", "--watch", "--backend", "vm", "--quit-after", "30s"])
            .env("KIRA_LIVE_NO_HOTPATCH", "1")
            .arg(path)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("kira spawns"),
    );
    let stdout = session.0.stdout.take().expect("stdout pipe");
    let stderr = session.0.stderr.take().expect("stderr pipe");
    let (lines_sender, lines_receiver) = mpsc::channel();
    let reader = thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else {
                break;
            };
            if lines_sender.send(line).is_err() {
                break;
            }
        }
    });
    let stderr_reader = thread::spawn(move || {
        let mut text = String::new();
        let mut stderr = stderr;
        let _ = stderr.read_to_string(&mut text);
        text
    });

    let deadline = Instant::now() + SESSION_TIMEOUT;
    let mut lines = Vec::new();
    let mut edit = Some(edit);
    while Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let Ok(line) = lines_receiver.recv_timeout(remaining) else {
            break;
        };
        if line == "live.watch.started"
            && let Some(edit) = edit.take()
        {
            edit();
        }
        lines.push(line);
        if done(&lines) {
            break;
        }
    }

    session.stop();
    let _ = reader.join();
    let stderr = stderr_reader.join().unwrap_or_default();
    (lines, stderr)
}

fn count(lines: &[String], prefix: &str) -> usize {
    lines.iter().filter(|line| line.starts_with(prefix)).count()
}

fn has_in_order(lines: &[String], expected: &[&str]) -> bool {
    let mut next = 0;
    for line in lines {
        if next < expected.len() && line.starts_with(expected[next]) {
            next += 1;
        }
    }
    next == expected.len()
}

const APP_BEFORE: &str = "@Main function main() { print(\"LIVE_BEFORE\") return }\n";
const APP_AFTER: &str = "@Main function main() { print(\"LIVE_AFTER_\") return }\n";
const APP_ATOMIC: &str = "@Main function main() { print(\"LIVE_ATOMIC\") return }\n";

#[test]
fn a_source_save_reaches_a_new_bundle_and_a_ready_runner() {
    let scratch = Scratch::new("sequence");
    let app = scratch.write("app.kira", APP_BEFORE);
    let edited = app.clone();
    let scratch_root = scratch.0.clone();

    let (lines, stderr) = live_until(
        &app,
        move || {
            std::fs::write(scratch_root.join("app.kira~"), "editor noise").expect("editor noise");
            std::fs::create_dir_all(scratch_root.join(".KIRA-BUILD"))
                .expect("build output directory");
            std::fs::write(scratch_root.join(".KIRA-BUILD/output"), "build output")
                .expect("build output");
            std::fs::write(&edited, APP_AFTER).expect("ordinary overwrite");
        },
        |lines| {
            count(lines, "live.session.ready") >= 2
                && count(lines, "live.bundle.sent") >= 2
                && count(lines, "live.runner.relaunched") >= 1
                && lines.iter().any(|line| line == "LIVE_AFTER_")
        },
    );

    assert!(
        has_in_order(
            &lines,
            &[
                "live.watch.started",
                "live.source.changed",
                "live.bundle.rebuilt",
                "live.bundle.sent",
                "live.session.ready",
                "live.runner.relaunched",
            ]
        ),
        "the edit must reach the rebuilt bundle and relaunched runner\nstdout:\n{}\nstderr:\n{stderr}",
        lines.join("\n")
    );
    assert_eq!(
        count(&lines, "live.source.changed"),
        1,
        "editor noise and build output must not create extra rebuilds\nstdout:\n{}",
        lines.join("\n")
    );
    assert!(
        lines.iter().any(|line| line == "LIVE_BEFORE")
            && lines.iter().any(|line| line == "LIVE_AFTER_"),
        "both runner launches must execute the app\nstdout:\n{}\nstderr:\n{stderr}",
        lines.join("\n")
    );
}

#[test]
fn an_atomic_replace_reaches_a_second_bundle_and_a_ready_runner() {
    let scratch = Scratch::new("atomic");
    let app = scratch.write("app.kira", APP_BEFORE);
    let edited = app.clone();

    let (lines, stderr) = live_until(
        &app,
        move || {
            std::fs::write(&edited, APP_AFTER).expect("ordinary edit");
            thread::sleep(Duration::from_secs(2));
            let replacement = edited.with_extension("kira.replace.tmp");
            std::fs::write(&replacement, APP_ATOMIC).expect("replacement");
            std::fs::rename(&replacement, &edited).expect("atomic replacement");
        },
        |lines| {
            count(lines, "live.session.ready") >= 3
                && count(lines, "live.bundle.sent") >= 3
                && count(lines, "live.runner.relaunched") >= 2
                && lines.iter().any(|line| line == "LIVE_ATOMIC")
        },
    );

    assert!(
        has_in_order(
            &lines,
            &[
                "live.watch.started",
                "live.source.changed",
                "live.bundle.rebuilt",
                "live.bundle.sent",
                "live.session.ready",
                "live.runner.relaunched",
                "live.source.changed",
                "live.bundle.rebuilt",
                "live.bundle.sent",
                "live.session.ready",
                "live.runner.relaunched",
            ]
        ),
        "ordinary and atomic saves must each reload the app\nstdout:\n{}\nstderr:\n{stderr}",
        lines.join("\n")
    );
    assert!(
        lines.iter().any(|line| line == "LIVE_BEFORE")
            && lines.iter().any(|line| line == "LIVE_AFTER_")
            && lines.iter().any(|line| line == "LIVE_ATOMIC"),
        "all three bundles must execute\nstdout:\n{}\nstderr:\n{stderr}",
        lines.join("\n")
    );
}

#[test]
fn an_imported_dependency_package_source_is_watched() {
    let scratch = Scratch::new("dependency");
    scratch.write(
        "editor/package.kira",
        "Package EditorApp {\n    let version = \"0.1.0\"\n    let kind = .App\n    let dependencies = [Dependency { name: \"Core\", path: \"../core\" }]\n}\n",
    );
    scratch.write(
        "core/package.kira",
        "Package Core {\n    let version = \"0.1.0\"\n    let kind = .Library\n    let moduleRoot = \"Core\"\n}\n",
    );
    let dependency = scratch.write(
        "core/app/Values.kira",
        "function coreValue() -> Int { return 41 }\n",
    );
    scratch.write(
        "editor/app/main.kira",
        "import Core\n@Main function main() { print(coreValue()) return }\n",
    );
    let edited = dependency.clone();

    let (lines, stderr) = live_until(
        &scratch.0.join("editor"),
        move || {
            std::fs::write(&edited, "function coreValue() -> Int { return 42 }\n")
                .expect("edit dependency source");
        },
        |lines| {
            count(lines, "live.session.ready") >= 2
                && lines.iter().any(|line| line == "42")
                && lines
                    .iter()
                    .any(|line| line.starts_with("live.runner.relaunched"))
        },
    );

    assert!(
        lines
            .iter()
            .any(|line| line.contains("core") && line.starts_with("live.source.changed")),
        "the dependency path must be the detected source\nstdout:\n{}\nstderr:\n{stderr}",
        lines.join("\n")
    );
    assert!(
        lines.iter().any(|line| line == "42"),
        "the relaunched bundle must execute the dependency edit\nstdout:\n{}\nstderr:\n{stderr}",
        lines.join("\n")
    );
}
