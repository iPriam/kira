//! Subprocess runner for devflow: run `git`/`gh` from a working directory and
//! capture their output. Thin wrapper over [`std::process::Command`] so every
//! module shares one consistent execution and error-reporting path.

use std::process::Command;

use crate::error::DevflowError;

/// The captured result of one command.
pub struct Output {
    /// Standard output, decoded lossily.
    pub stdout: String,
    /// Standard error, decoded lossily.
    pub stderr: String,
    /// The exit code, or `None` on a signal / abnormal termination.
    pub code: Option<i32>,
    /// Whether the command exited zero.
    pub success: bool,
}

/// Run `argv` with `cwd` as working directory and capture stdout/stderr.
///
/// Only an inability to spawn the process is an error here; a non-zero exit is
/// reported through [`Output`] so probes that legitimately fail can inspect it.
pub fn run(cwd: &str, argv: &[&str]) -> Result<Output, DevflowError> {
    let output = Command::new(argv[0])
        .args(&argv[1..])
        .current_dir(cwd)
        .output()
        .map_err(|error| {
            DevflowError::msg(format!("could not run `{}`: {error}", argv.join(" ")))
        })?;
    Ok(Output {
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        code: output.status.code(),
        success: output.status.success(),
    })
}

/// Run a command that must succeed; return its trimmed stdout.
pub fn capture(cwd: &str, argv: &[&str]) -> Result<String, DevflowError> {
    let output = run(cwd, argv)?;
    if !output.success {
        return Err(command_failure(argv, &output));
    }
    Ok(output.stdout.trim().to_string())
}

/// Run a command that must succeed; discard stdout, surface stderr on failure.
pub fn check(cwd: &str, argv: &[&str]) -> Result<(), DevflowError> {
    let output = run(cwd, argv)?;
    if !output.success {
        return Err(command_failure(argv, &output));
    }
    Ok(())
}

/// Format a non-zero exit as the error the caller propagates.
fn command_failure(argv: &[&str], output: &Output) -> DevflowError {
    let exit = match output.code {
        Some(code) => format!("exit {code}"),
        None => String::from("abnormal termination"),
    };
    let mut message = format!("command failed: {} ({exit})", argv.join(" "));
    let stderr = output.stderr.trim();
    if !stderr.is_empty() {
        message.push('\n');
        message.push_str(stderr);
    }
    DevflowError::Message(message)
}
