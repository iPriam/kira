//! The `kira` launcher: resolves the installed toolchain and dispatches to its
//! primary binary, the compiler CLI that `kira-cli` builds as `kira`.
//!
//! Standalone tool crate (outside the layered package graph). Resolution reads
//! `<kira-home>/toolchains/current.toml` (see `kira-toolchain`) and executes the
//! selected primary binary, forwarding every argument and the whole environment
//! untouched.
//!
//! # Multi-call: one binary, two names
//!
//! Installed as `kira`, it dispatches to the selected toolchain's primary
//! binary. Installed as `kira-language-server` (a second copy `sinstall`
//! lands beside it), it dispatches to the selected toolchain's language
//! server instead. That is what puts a *version-following* server on PATH:
//! an editor spawns `kira-language-server`, and what runs is always the
//! server of the toolchain `knvm` has selected — never a copy frozen at
//! whatever the language looked like on install day.
//!
//! Exit code 2 means exactly one thing: the launcher itself could not dispatch.
//! Once dispatch succeeds the exit status belongs to the toolchain binary — on
//! unix by construction, because the process image is replaced.

use std::path::PathBuf;
use std::process::Command;

use kira_toolchain::{
    CurrentToolchain, HomeDirectoryUnavailable, InvalidCurrentToolchain, PinError,
};

/// Process exit code for "the launcher could not dispatch".
const EXIT_LAUNCHER_FAILED: i32 = 2;

/// The primary binary a pin implies.
///
/// A pin names a toolchain, never a binary inside it; which binary to run is
/// decided from `argv[0]` exactly as it is for the global selection.
const PINNED_PRIMARY: &str = "kira";

/// Everything that stops the launcher before the toolchain binary starts.
#[derive(Debug, thiserror::Error)]
enum LaunchError {
    /// Neither `KIRA_HOME` nor a home directory is available.
    #[error("{0}")]
    HomeUnavailable(#[from] HomeDirectoryUnavailable),
    /// No toolchain has been selected: `current.toml` does not exist.
    #[error("no toolchain selected ({} does not exist)", path.display())]
    NoToolchainSelected { path: PathBuf },
    /// `current.toml` exists but could not be read.
    #[error("cannot read {}: {source}", path.display())]
    UnreadableState {
        path: PathBuf,
        source: std::io::Error,
    },
    /// `current.toml` exists but is not valid current-toolchain state.
    #[error("{}: {source}", path.display())]
    MalformedState {
        path: PathBuf,
        source: InvalidCurrentToolchain,
    },
    /// A `kira-toolchain.toml` was found and could not be read as a pin.
    #[error("{0}")]
    UnreadablePin(#[from] PinError),
    /// The pinned toolchain is not installed.
    #[error(
        "`{}` pins {channel}/{version}, which is not installed",
        pin.display()
    )]
    PinnedToolchainMissing {
        pin: PathBuf,
        channel: &'static str,
        version: String,
    },
    /// The selected toolchain does not carry the primary binary it names.
    #[error(
        "selected toolchain {channel}/{version} has no `{primary}` at {}",
        path.display()
    )]
    MissingPrimaryBinary {
        channel: &'static str,
        version: String,
        primary: String,
        path: PathBuf,
    },
    /// The primary binary exists but could not be executed.
    #[error("cannot execute {}: {source}", path.display())]
    NotExecuted {
        path: PathBuf,
        source: std::io::Error,
    },
}

impl LaunchError {
    /// The one-line remedy printed under the diagnostic, when there is one.
    fn remedy(&self) -> Option<&'static str> {
        match self {
            Self::NoToolchainSelected { .. } => {
                Some("install one with `knvm install latest`, then re-run `kira`")
            }
            Self::MalformedState { .. } | Self::MissingPrimaryBinary { .. } => {
                Some("repair it with `knvm install latest`")
            }
            Self::PinnedToolchainMissing { .. } => {
                Some("install it with `knvm install <version>`, or drop the pin with `knvm unpin`")
            }
            Self::UnreadablePin(_) => Some("fix the pin, or drop it with `knvm unpin`"),
            Self::HomeUnavailable(_) | Self::UnreadableState { .. } | Self::NotExecuted { .. } => {
                None
            }
        }
    }
}

fn main() {
    match run() {
        // `run` only returns on failure: the success path either replaces this
        // process image (unix) or exits with the child's code (windows).
        Ok(never) => match never {},
        Err(error) => {
            eprintln!("kira: {error}");
            if let Some(remedy) = error.remedy() {
                eprintln!("kira: {remedy}");
            }
            std::process::exit(EXIT_LAUNCHER_FAILED);
        }
    }
}

/// Resolves the selected toolchain and hands the process over to it.
fn run() -> Result<std::convert::Infallible, LaunchError> {
    let selected = resolve_toolchain()?;
    let target = dispatch_target(&selected);
    let binary =
        kira_toolchain::managed_primary_binary_path(selected.channel, &selected.version, &target)?;
    if !binary.is_file() {
        return Err(LaunchError::MissingPrimaryBinary {
            channel: selected.channel.dir_name(),
            version: selected.version,
            primary: target,
            path: binary,
        });
    }
    dispatch(binary)
}

/// The file stem `argv[0]` was invoked under (`.exe` stripped on Windows).
fn invoked_stem() -> Option<String> {
    let invoked_as = std::env::args_os().next().map(PathBuf::from)?;
    invoked_as
        .file_stem()
        .and_then(|stem| stem.to_str())
        .map(str::to_owned)
}

/// The toolchain binary this invocation dispatches to, chosen by `argv[0]`.
///
/// Invoked under the language server's name, the launcher is the language
/// server's proxy; under any other name — `kira`, a renamed copy, an argv[0]
/// a caller made up — it dispatches to the selected primary, which is the
/// launcher's one job and the only safe reading of an unrecognized name.
fn dispatch_target(selected: &CurrentToolchain) -> String {
    match invoked_stem().as_deref() {
        Some(kira_toolchain::LANGUAGE_SERVER_BINARY) => {
            kira_toolchain::LANGUAGE_SERVER_BINARY.to_string()
        }
        _ => selected.primary.clone(),
    }
}

/// The toolchain this invocation runs: a directory tree's pin, else the
/// global selection.
///
/// The pin wins because it is the more specific statement — the same reason a
/// project's `rust-toolchain.toml` outranks the default toolchain. A pin whose
/// toolchain is not installed is refused here rather than falling back: falling
/// back would run a compiler the project explicitly said it does not want, and
/// would do it silently.
fn resolve_toolchain() -> Result<CurrentToolchain, LaunchError> {
    let Ok(working_directory) = std::env::current_dir() else {
        // No working directory to walk up from — a deleted cwd, or a caller
        // that unset it. There is no pin to find, so the global selection is
        // the whole answer.
        return read_selection();
    };
    let Some(pin) = kira_toolchain::find_pin(&working_directory)? else {
        return read_selection();
    };

    let root = kira_toolchain::managed_toolchain_root(pin.channel, &pin.version)?;
    if !root.is_dir() {
        return Err(LaunchError::PinnedToolchainMissing {
            pin: pin.path,
            channel: pin.channel.dir_name(),
            version: pin.version,
        });
    }
    Ok(CurrentToolchain {
        channel: pin.channel,
        version: pin.version,
        primary: PINNED_PRIMARY.to_string(),
    })
}

/// Reads and parses `current.toml`, distinguishing "absent" from "broken".
fn read_selection() -> Result<CurrentToolchain, LaunchError> {
    let path = kira_toolchain::current_toolchain_path()?;
    let contents = match std::fs::read_to_string(&path) {
        Ok(contents) => contents,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
            return Err(LaunchError::NoToolchainSelected { path });
        }
        Err(source) => return Err(LaunchError::UnreadableState { path, source }),
    };
    CurrentToolchain::parse_toml(&contents)
        .map_err(|source| LaunchError::MalformedState { path, source })
}

/// Every argument after `argv[0]`, forwarded to the toolchain binary untouched.
fn forwarded_arguments() -> Vec<std::ffi::OsString> {
    std::env::args_os().skip(1).collect()
}

/// Replaces this process with the toolchain binary.
///
/// The exit status, signal disposition, and stdio are the toolchain binary's by
/// construction: there is no launcher process left to translate them.
#[cfg(unix)]
fn dispatch(binary: PathBuf) -> Result<std::convert::Infallible, LaunchError> {
    use std::os::unix::process::CommandExt;

    let source = Command::new(&binary).args(forwarded_arguments()).exec();
    Err(LaunchError::NotExecuted {
        path: binary,
        source,
    })
}

/// Runs the toolchain binary and exits with its code.
///
/// Windows has no `exec`, so the launcher stays alive as a parent and forwards
/// the child's exit code. A child killed by the system reports no exit code;
/// report that as a generic failure rather than as launcher failure, since the
/// toolchain binary did run.
#[cfg(windows)]
fn dispatch(binary: PathBuf) -> Result<std::convert::Infallible, LaunchError> {
    let status = Command::new(&binary)
        .args(forwarded_arguments())
        .status()
        .map_err(|source| LaunchError::NotExecuted {
            path: binary,
            source,
        })?;
    std::process::exit(status.code().unwrap_or(1));
}
