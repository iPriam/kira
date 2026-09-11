//! Running a shell command, read back through the request surface.
//!
//! A harness that edits code also has to run it — a build, a test, a formatter —
//! and read what it printed. That is the same shape as a request: something is
//! started, it finishes with a code, and it leaves output to read. So a command
//! is registered as an operation like a request is, and its exit code and
//! captured output are handed back as a [`ResponseData`] — the caller polls,
//! reads the status for the exit code, and reads the body for the output, with
//! the functions it already uses for a response.
//!
//! Foreground and background are the same operation: a caller that waits for the
//! poll to be ready ran it in the foreground, and one that returns the handle
//! and reads it later ran it in the background. The distinction is the caller's,
//! not the surface's.

use crate::request::ResponseData;
use crate::runtime::{self, NetworkError, OperationId};

/// Runs `command` with `/bin/sh -c`, capturing stdout and stderr together and
/// its exit code.
///
/// The blocking `std::process::Command` runs on a blocking thread so it never
/// stalls the async reactor. `cwd` empty means the harness's own directory.
async fn run(command: String, cwd: String) -> Result<ResponseData, NetworkError> {
    let output = tokio::task::spawn_blocking(move || {
        let mut process = std::process::Command::new("/bin/sh");
        process.arg("-c").arg(&command);
        if !cwd.is_empty() {
            process.current_dir(&cwd);
        }
        process.output()
    })
    .await
    .map_err(|_| NetworkError::Io)?
    .map_err(|_| NetworkError::Io)?;

    // stdout then stderr, so a caller reading the body sees what the command
    // printed in the order a terminal would have shown it closely enough for a
    // person, and a tool that only wants the answer still finds it.
    let mut body = output.stdout;
    body.extend_from_slice(&output.stderr);

    // A process that a signal ended has no code; `137`-style shell convention
    // (128 + signal) is more than a `u16` needs, so an absent code is reported
    // as `255`, the shell's own "failed, no better number" — never as success.
    let status = match output.status.code() {
        Some(code) if (0..=255).contains(&code) => code as u16,
        _ => 255,
    };
    Ok(ResponseData::from_output(status, body))
}

/// Starts a command and returns its operation handle. The caller polls it,
/// reads the status for the exit code, and the body for the output.
pub fn start(command: &str, cwd: &str) -> Result<OperationId, NetworkError> {
    runtime::register_request(run(command.to_owned(), cwd.to_owned()))
}
