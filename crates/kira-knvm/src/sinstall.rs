//! Installing knvm itself out of a checkout: `knvm sinstall`.
//!
//! `binstall` provisions a *toolchain*; this provisions the *tools* — `knvm`,
//! the `kira` launcher, and the launcher's `kira-language-server` alias — which
//! live outside any toolchain version because they are what selects between
//! versions. They are built with cargo from the
//! enclosing checkout and landed in `<kira-home>/bin`, and the host is pointed
//! at that directory the way rustup points one at `~/.cargo/bin` — by a
//! sourced `env` script on unix, and by the user's own `Path` in the registry
//! on Windows. `path_setup` owns that half.
//!
//! Everything here is idempotent. The binaries are replaced by staging beside
//! the destination and renaming, and the PATH entry is added only where it is
//! not already present.
//!
//! # Replacing the running `knvm`
//!
//! This command's whole job is to replace the executable that is running it.
//! Unix lets a rename land on a busy binary; Windows refuses to open the file
//! that backs a running image for delete, so a rename *onto* it fails with
//! access denied — which is `sinstall` failing at exactly the step it exists
//! for. Windows does allow the running image to be renamed *away*, so that is
//! what happens: the old tool is moved aside, the new one takes its name, and
//! the displaced file is deleted now if it can be and on the next run if it
//! cannot.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::binstall::{BinstallError, cargo_target_dir, enclosing_checkout};
use crate::install::InstallError;
use crate::path_setup::{self, PathConfigured};

/// The tools this installs, as `(built binary, installed name)`.
///
/// The launcher lands twice: once as `kira`, and once under the language
/// server's name, where it dispatches to the selected toolchain's server
/// instead of its primary (see the multi-call note in `kira-launcher`).
/// PATH then always resolves the *selected* server, never a frozen copy.
///
/// It is built as `kira-launcher` and installed as `kira`, which is the one
/// place those two names differ. The compiler CLI is also built as `kira` — one
/// cargo workspace cannot produce two binaries of one name — and it is the
/// *toolchain's* `kira`, installed under `toolchains/<version>/bin/`. What
/// lands on PATH here is the launcher, which resolves the selected toolchain
/// and execs that one.
const TOOLS: [(&str, &str); 3] = [
    ("knvm", "knvm"),
    (LAUNCHER_BINARY, "kira"),
    (LAUNCHER_BINARY, kira_toolchain::LANGUAGE_SERVER_BINARY),
];

/// The launcher's name as cargo builds it.
const LAUNCHER_BINARY: &str = "kira-launcher";

/// What a self-install did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelfInstalled {
    /// Where the tools were installed.
    pub bin_dir: PathBuf,
    /// How this host was pointed at `bin_dir`.
    pub path: PathConfigured,
}

/// Builds `knvm` and `kira` from the enclosing checkout and installs them.
///
/// `kira_home` is where the tools land (`<kira_home>/bin`) and where the unix
/// `env` script is written. `shell_home` is the directory whose startup file is
/// edited — the user's home, passed explicitly so tests drive a throwaway one.
/// `shell` is the user's shell (`$SHELL`), which decides *which* startup file:
/// a line in a file the shell never reads configures nothing. Both are unix's
/// business; Windows configures the user's registry environment instead, which
/// is not a file and takes neither. `start` is where the checkout search
/// begins.
pub fn sinstall(
    kira_home: &Path,
    shell_home: &Path,
    shell: Option<&str>,
    start: &Path,
) -> Result<SelfInstalled, BinstallError> {
    let checkout = enclosing_checkout(start).ok_or_else(|| BinstallError::NotACheckout {
        start: start.to_path_buf(),
    })?;
    let target_dir = cargo_target_dir(&checkout)?;

    let built = Command::new("cargo")
        .args(["build", "-p", "kira-knvm", "-p", "kira-launcher"])
        .current_dir(&checkout)
        .status()
        .map_err(|error| match error.kind() {
            std::io::ErrorKind::NotFound => BinstallError::CargoUnavailable,
            _ => BinstallError::BuildFailed {
                checkout: checkout.clone(),
            },
        })?;
    if !built.success() {
        return Err(BinstallError::BuildFailed { checkout });
    }

    let bin_dir = kira_home.join("bin");
    std::fs::create_dir_all(&bin_dir)
        .map_err(|error| InstallError::io("create", &bin_dir, error))?;

    sweep_displaced(&bin_dir);

    let debug_dir = target_dir.join("debug");
    for (tool, installed) in TOOLS {
        let name = kira_toolchain::executable_name(installed);
        let built_binary = debug_dir.join(kira_toolchain::executable_name(tool));
        if !built_binary.is_file() {
            return Err(BinstallError::MissingBuildArtifact {
                expected: built_binary,
            });
        }
        let staged = bin_dir.join(format!("{DISPLACED_PREFIX}incoming-{name}"));
        std::fs::copy(&built_binary, &staged)
            .map_err(|error| InstallError::io("copy the tool to", &staged, error))?;
        let destination = bin_dir.join(&name);
        displace(&destination)?;
        std::fs::rename(&staged, &destination)
            .map_err(|error| InstallError::io("move the tool into", &destination, error))?;
    }

    let path = path_setup::configure(kira_home, &bin_dir, shell_home, shell)?;

    Ok(SelfInstalled { bin_dir, path })
}

/// The prefix every staged and displaced file carries, so a sweep can tell
/// this command's leavings from an installed tool by name alone.
const DISPLACED_PREFIX: &str = ".knvm-";

/// Moves an installed tool out of the way of its replacement.
///
/// A tool that is not installed yet needs no room made. One that is running —
/// the `knvm` performing this install, every time — is renamed rather than
/// deleted, because the running image holds the file open under its old name
/// and the new name is what the next run will resolve.
fn displace(destination: &Path) -> Result<(), BinstallError> {
    if !destination.exists() {
        return Ok(());
    }
    let Some(name) = destination.file_name().and_then(|name| name.to_str()) else {
        return Err(InstallError::io(
            "read the name of",
            destination,
            std::io::Error::from(std::io::ErrorKind::InvalidInput),
        )
        .into());
    };
    let Some(parent) = destination.parent() else {
        return Err(InstallError::io(
            "read the directory of",
            destination,
            std::io::Error::from(std::io::ErrorKind::InvalidInput),
        )
        .into());
    };
    let displaced = parent.join(format!(
        "{DISPLACED_PREFIX}old-{name}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&displaced);
    std::fs::rename(destination, &displaced).map_err(|error| {
        InstallError::io("move the installed tool aside from", destination, error)
    })?;
    // The running image cannot be deleted until it exits, and it is this
    // process: the next run's sweep is what clears it.
    let _ = std::fs::remove_file(&displaced);
    Ok(())
}

/// Deletes what earlier installs left behind, as far as the host allows.
fn sweep_displaced(bin_dir: &Path) {
    let Ok(entries) = std::fs::read_dir(bin_dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if name.starts_with(DISPLACED_PREFIX) {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}
