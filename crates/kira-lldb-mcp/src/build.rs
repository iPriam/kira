//! Building the target a session debugs.
//!
//! A Kira source file is not something LLDB can open. `kira debug --prepare`
//! compiles it, keeps the artifacts, and prints the executable, the arguments
//! that host it, and the function identities a breakpoint resolves against.
//! This runs that command and reads its answer, so the compiler stays the only
//! thing that knows how a Kira program is built.

use std::path::{Path, PathBuf};
use std::process::Command;

use kira_debug::PreparedTarget;

/// The environment variable naming the `kira` executable to build with.
pub const KIRA_VARIABLE: &str = "KIRA_EXECUTABLE";

/// How a program was selected for debugging.
pub enum Program<'a> {
    /// A Kira source file or package directory to compile first.
    Source {
        /// The path `kira debug` is pointed at.
        path: &'a str,
        /// The backend to build for.
        backend: &'a str,
        /// Whether to build the native unit optimized.
        release: bool,
        /// Kira function breakpoints the built host should honour.
        breakpoints: &'a [String],
        /// Arguments passed to the debugged program.
        arguments: &'a [String],
    },
    /// An executable that is already built and needs no compiler.
    Executable {
        /// The file LLDB opens.
        path: &'a str,
        /// Arguments passed to it.
        arguments: &'a [String],
    },
}

/// Produces the target description a session is started from.
pub fn prepare(program: &Program<'_>) -> Result<PreparedTarget, String> {
    match program {
        Program::Source {
            path,
            backend,
            release,
            breakpoints,
            arguments,
        } => compile(path, backend, *release, breakpoints, arguments),
        Program::Executable { path, arguments } => Ok(prebuilt(path, arguments)),
    }
}

/// Runs `kira debug --prepare` and parses the target it describes.
fn compile(
    path: &str,
    backend: &str,
    release: bool,
    breakpoints: &[String],
    arguments: &[String],
) -> Result<PreparedTarget, String> {
    let executable = kira_executable();
    let mut command = Command::new(&executable);
    command.arg("debug").arg(path);
    command.arg("--backend").arg(backend);
    command.arg("--prepare");
    if release {
        command.arg("--release");
    }
    for breakpoint in breakpoints {
        command.arg("--break").arg(breakpoint);
    }
    if !arguments.is_empty() {
        command.arg("--");
        command.args(arguments);
    }
    let output = command.output().map_err(|error| {
        format!(
            "cannot run `{}` to build `{path}`: {error}",
            executable.display()
        )
    })?;
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    if !output.status.success() {
        return Err(match stderr.trim().is_empty() {
            true => format!("`kira debug --prepare` failed for `{path}`"),
            false => stderr.trim().to_owned(),
        });
    }
    // The description is the command's result and the last thing it prints;
    // anything before it is progress the compiler wrote for a human.
    let description = stdout
        .lines()
        .rev()
        .find(|line| line.trim_start().starts_with('{'))
        .ok_or_else(|| match stderr.trim().is_empty() {
            true => format!("`kira debug --prepare` described no target for `{path}`"),
            false => format!(
                "`kira debug --prepare` described no target: {}",
                stderr.trim()
            ),
        })?;
    serde_json::from_str(description)
        .map_err(|error| format!("cannot read the prepared target for `{path}`: {error}"))
}

/// Describes an executable that was built outside the compiler.
///
/// It has no Kira function table, which is exactly what a caller should see:
/// breakpoints on it are native symbols and source lines, and nothing pretends
/// to know a bytecode identity that was never emitted.
fn prebuilt(path: &str, arguments: &[String]) -> PreparedTarget {
    PreparedTarget {
        backend: "native".to_owned(),
        source: PathBuf::new(),
        module_name: Path::new(path)
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_else(|| "program".to_owned()),
        executable: PathBuf::from(path),
        arguments: arguments.to_vec(),
        optimized: false,
        functions: Vec::new(),
        probe: None,
        artifacts: Vec::new(),
    }
}

/// The `kira` executable this server builds with.
fn kira_executable() -> PathBuf {
    std::env::var_os(KIRA_VARIABLE)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("kira"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_prebuilt_executable_needs_no_compiler_and_claims_no_kira_identities() {
        let target = prebuilt("build/demo.exe", &["--verbose".to_owned()]);
        assert_eq!(target.backend, "native");
        assert_eq!(target.module_name, "demo");
        assert_eq!(target.arguments, vec!["--verbose".to_owned()]);
        assert!(target.functions.is_empty());
        assert!(target.probe.is_none());
        assert!(target.artifacts.is_empty());
    }

    #[test]
    fn the_build_executable_is_overridable_for_a_workspace_build() {
        assert_eq!(KIRA_VARIABLE, "KIRA_EXECUTABLE");
        assert!(!kira_executable().as_os_str().is_empty());
    }
}
