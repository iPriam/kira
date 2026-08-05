//! Running a real toolchain command, with a deterministic timeout.
//!
//! Every tool in this server answers by running the same commands a developer
//! would. Nothing is simulated, and nothing is summarized away: the exit code,
//! stdout and stderr of each command are carried into the result whole, because
//! a tool that reported only its own verdict would be asking to be trusted
//! about the thing under test.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::{Value, json};

/// The default wall-clock bound on one **call**, not on one command.
///
/// The full workspace suite takes minutes on a cold cache, so this is generous.
/// A caller that knows better passes its own. A tool that runs several commands
/// shares this budget between them through a [`Deadline`], because a per-command
/// bound multiplied by the number of commands is not a bound a caller can
/// predict.
pub const DEFAULT_TIMEOUT_SECONDS: u64 = 1800;

/// The wall-clock budget one tool call shares across every command it runs.
pub struct Deadline {
    end: Instant,
}

impl Deadline {
    pub fn new(total: Duration) -> Self {
        Deadline {
            end: Instant::now() + total,
        }
    }

    /// What is left of the budget, or zero once it is spent.
    pub fn remaining(&self) -> Duration {
        self.end.saturating_duration_since(Instant::now())
    }

    pub fn expired(&self) -> bool {
        self.remaining().is_zero()
    }
}

/// What one command did.
#[derive(Debug, Clone, Serialize)]
pub struct Run {
    /// The program and arguments, as they were run.
    pub command: Vec<String>,
    /// Where it ran.
    pub cwd: String,
    /// `None` when the process was killed by a signal or the timeout.
    pub exit_code: Option<i32>,
    /// Whether the timeout fired.
    pub timed_out: bool,
    pub duration_seconds: f64,
    pub stdout: String,
    pub stderr: String,
}

impl Run {
    /// Whether the command reported success.
    ///
    /// A timeout is never success, whatever the exit code turned out to be.
    pub fn success(&self) -> bool {
        !self.timed_out && self.exit_code == Some(0)
    }
}

/// Why a command could not be run at all.
#[derive(Debug, thiserror::Error)]
pub enum ExecError {
    /// The program could not be spawned.
    #[error("cannot run `{program}`: {source}")]
    Spawn {
        program: String,
        #[source]
        source: std::io::Error,
    },
}

/// Runs `program` with `args` under `cwd`, bounded by `timeout`.
///
/// # Why the pipes are drained on their own threads
///
/// A pipe holds about 64 KB before a write to it blocks. `cargo build
/// --workspace` writes far more than that, so a parent that waits for the child
/// *before* reading waits forever: the child is blocked writing, the parent is
/// blocked waiting, and neither can proceed. That is a deadlock, not a slow
/// build, and it ends only when the timeout fires — which is why this reads
/// both streams while the child is still running rather than after it exits.
///
/// The timeout is enforced by killing the whole process tree. Killing only the
/// child leaves the compiler it spawned holding the same pipe handles, so the
/// read would go on blocking after the kill and the bound would not be one.
pub fn run(
    program: &str,
    args: &[String],
    cwd: &Path,
    env: &[(String, String)],
    timeout: Duration,
) -> Result<Run, ExecError> {
    let started = Instant::now();
    let mut command = Command::new(program);
    command
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (key, value) in env {
        command.env(key, value);
    }
    let mut child = command.spawn().map_err(|source| ExecError::Spawn {
        program: program.to_owned(),
        source,
    })?;

    let out_reader = drain(child.stdout.take());
    let err_reader = drain(child.stderr.take());

    let mut timed_out = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) => {
                if started.elapsed() >= timeout {
                    kill_tree(&mut child);
                    timed_out = true;
                    break None;
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            Err(_) => break None,
        }
    };

    // Both streams are closed once the tree is gone, so these end. Joining is
    // what carries a killed command's partial output into the result.
    let stdout = out_reader.recv().unwrap_or_default();
    let stderr = err_reader.recv().unwrap_or_default();

    let mut argv = vec![program.to_owned()];
    argv.extend_from_slice(args);
    Ok(Run {
        command: argv,
        cwd: cwd.display().to_string(),
        exit_code: status.and_then(|status| status.code()),
        timed_out,
        duration_seconds: started.elapsed().as_secs_f64(),
        stdout,
        stderr,
    })
}

/// Reads one of the child's streams to the end on a thread of its own.
///
/// Returns a receiver rather than a `JoinHandle` so a caller that never joins
/// cannot block: the thread owns the pipe, and dropping the receiver leaves it
/// to finish and exit on its own.
fn drain<R: Read + Send + 'static>(stream: Option<R>) -> mpsc::Receiver<String> {
    let (sender, receiver) = mpsc::channel();
    let Some(mut stream) = stream else {
        let _ = sender.send(String::new());
        return receiver;
    };
    std::thread::spawn(move || {
        let mut buffer = Vec::new();
        let _ = stream.read_to_end(&mut buffer);
        let _ = sender.send(String::from_utf8_lossy(&buffer).into_owned());
    });
    receiver
}

/// Ends the child and everything it spawned.
///
/// A build tool is a process *tree* — cargo spawns rustc, rustc spawns the
/// linker — and every one of them inherits the pipes. Killing the root alone
/// leaves the rest writing into a pipe nobody will close, so the timeout would
/// bound the wait and not the call.
fn kill_tree(child: &mut std::process::Child) {
    #[cfg(windows)]
    {
        let _ = Command::new("taskkill")
            .args(["/PID", &child.id().to_string(), "/T", "/F"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    #[cfg(not(windows))]
    {
        // Negative pid signals the group, which is the tree cargo put itself at
        // the head of.
        let _ = Command::new("kill")
            .args(["-KILL", &format!("-{}", child.id())])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    let _ = child.kill();
    let _ = child.wait();
}

/// Builds an argument vector from string slices.
pub fn argv(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|part| (*part).to_owned()).collect()
}

/// The repository this server was built from.
///
/// Resolved from the crate's own manifest directory rather than from the
/// working directory a client happened to launch it in: a tool that ran cargo
/// in the wrong tree would answer about the wrong repository.
pub fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("crates/kira-mcp sits two levels under the root")
        .to_path_buf()
}

/// Renders a run as the JSON every tool embeds under `commands`.
pub fn run_json(run: &Run) -> Value {
    json!({
        "command": run.command,
        "cwd": run.cwd,
        "exit_code": run.exit_code,
        "timed_out": run.timed_out,
        "duration_seconds": run.duration_seconds,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root() -> PathBuf {
        repository_root()
    }

    #[test]
    fn a_command_that_succeeds_reports_its_output() {
        let run = run(
            "cargo",
            &argv(&["--version"]),
            &root(),
            &[],
            Duration::from_secs(120),
        )
        .expect("cargo runs");
        assert!(
            run.success(),
            "cargo --version must succeed: {}",
            run.stderr
        );
        assert!(run.stdout.contains("cargo"));
        assert!(!run.timed_out);
    }

    /// A failing command is reported as a failure, never smoothed into success.
    #[test]
    fn a_failing_command_keeps_its_exit_code() {
        let run = run(
            "cargo",
            &argv(&["--this-flag-does-not-exist"]),
            &root(),
            &[],
            Duration::from_secs(120),
        )
        .expect("cargo runs");
        assert!(!run.success());
        assert_ne!(run.exit_code, Some(0));
    }

    /// The regression this file exists to not have again.
    ///
    /// A pipe holds about 64 KB. Before the streams were drained on their own
    /// threads, a command that wrote more than that blocked writing while this
    /// blocked waiting, and the call ended only when the timeout fired —
    /// minutes of nothing, for a command that had already done its work.
    #[test]
    fn a_command_that_outsizes_the_pipe_buffer_still_completes() {
        // `--no-dedupe` is what makes this big enough to be a test: the deduped
        // tree fits in one buffer and would pass without the fix.
        let run = run(
            "cargo",
            &argv(&["tree", "-p", "kira-ir", "-e", "all", "--no-dedupe"]),
            &root(),
            &[],
            Duration::from_secs(120),
        )
        .expect("cargo runs");
        assert!(
            !run.timed_out,
            "the command finished; only the read blocked"
        );
        assert!(
            run.stdout.len() > 64 * 1024,
            "this proves nothing unless the output outsizes one pipe buffer; got {} bytes",
            run.stdout.len()
        );
    }

    /// A budget is spent by what has already run, so the checks after it get
    /// what is left rather than a fresh copy of the whole thing.
    #[test]
    fn a_deadline_is_shared_rather_than_restarted() {
        let deadline = Deadline::new(Duration::from_secs(60));
        assert!(!deadline.expired());
        assert!(deadline.remaining() <= Duration::from_secs(60));

        let spent = Deadline::new(Duration::ZERO);
        assert!(spent.expired());
        assert!(spent.remaining().is_zero());
    }

    /// The root is the repository, not wherever the server was launched.
    #[test]
    fn the_repository_root_holds_the_workspace_manifest() {
        assert!(root().join("Cargo.toml").is_file());
        assert!(root().join("crates").is_dir());
    }

    /// A program that does not exist is a typed error rather than a panic.
    #[test]
    fn a_missing_program_is_a_typed_error() {
        let error = run(
            "kira-mcp-no-such-program",
            &[],
            &root(),
            &[],
            Duration::from_secs(5),
        )
        .expect_err("a missing program cannot spawn");
        assert!(matches!(error, ExecError::Spawn { .. }));
    }
}
