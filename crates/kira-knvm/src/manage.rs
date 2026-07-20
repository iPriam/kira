//! Operating on toolchains that are already installed: list, select, remove.
//!
//! Like [`install`](crate::install), every operation takes the toolchains root
//! as an argument and never resolves `HOME` itself, so a test drives the shipped
//! code path against a throwaway directory without touching the process
//! environment.
//!
//! # What these never touch
//!
//! `llvm/` and `libffi/` are version-independent siblings shared across
//! toolchain versions, and `.staging/` belongs to a concurrent install.
//! [`uninstall`] removes exactly `<channel>/<version>/`, and clears
//! `current.toml` only when it named the version being removed. A version that
//! is not a single path component is refused rather than joined, so no argument
//! can walk out of its channel directory.

use std::path::{Path, PathBuf};

use kira_toolchain::{Channel, CurrentToolchain, executable_name};

use crate::install::{
    PRIMARY_BINARY, current_toolchain_path, is_single_component, read_current, toolchain_root,
    write_current,
};
use crate::source::sort_newest_first;

/// Why an operation on an installed toolchain could not be completed.
#[derive(Debug, thiserror::Error)]
pub enum ManageError {
    /// The named version is not installed on that channel.
    #[error("`{version}` is not installed on the `{channel}` channel (looked in `{}`)", .expected.display())]
    NotInstalled {
        /// The channel directory name.
        channel: &'static str,
        /// The version that was named.
        version: String,
        /// Where it would have been.
        expected: PathBuf,
    },
    /// The version is installed but is missing the binary the launcher runs.
    #[error("`{channel}` `{version}` is installed but has no `{}`; reinstall it", .expected.display())]
    Incomplete {
        /// The channel directory name.
        channel: &'static str,
        /// The version that was named.
        version: String,
        /// Where the primary binary was expected.
        expected: PathBuf,
    },
    /// The version argument is not a single directory name.
    #[error("`{version}` is not a version name; a version is one path component")]
    InvalidVersion {
        /// What was given.
        version: String,
    },
    /// Reading or writing `current.toml` failed.
    ///
    /// Selection is shared with installing, so its failures are reported in the
    /// install layer's own words rather than restated here.
    #[error(transparent)]
    Selection(#[from] crate::install::InstallError),
    /// A filesystem operation failed.
    #[error("could not {operation} `{}`: {source}", .path.display())]
    Io {
        /// What was being attempted.
        operation: &'static str,
        /// The path it was attempted on.
        path: PathBuf,
        /// The underlying error.
        source: std::io::Error,
    },
}

impl ManageError {
    /// An [`Io`](Self::Io) error carrying the path it happened on.
    fn io(operation: &'static str, path: &Path, source: std::io::Error) -> Self {
        Self::Io {
            operation,
            path: path.to_path_buf(),
            source,
        }
    }
}

/// One locally installed toolchain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledToolchain {
    /// The channel it is installed on.
    pub channel: Channel,
    /// Its version, which is its directory name.
    pub version: String,
    /// `<toolchains-root>/<channel>/<version>`.
    pub root: PathBuf,
    /// Whether `current.toml` names this one.
    pub is_current: bool,
    /// Whether `bin/<primary>` is present — a tree that lost it cannot be
    /// dispatched to, and is listed as broken rather than hidden.
    pub is_complete: bool,
}

/// Every locally installed toolchain, grouped by channel and newest first.
///
/// A toolchains root that does not exist yet has nothing installed, which is not
/// an error: it is the state of a machine that has never run `knvm install`.
pub fn list(toolchains_root: &Path) -> Result<Vec<InstalledToolchain>, ManageError> {
    let current = read_current(toolchains_root)?;
    let mut installed = Vec::new();

    for channel in Channel::ALL {
        let directory = toolchains_root.join(channel.dir_name());
        let entries = match std::fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(ManageError::io("read", &directory, error)),
        };

        let mut versions = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|error| ManageError::io("read", &directory, error))?;
            let is_directory = entry
                .file_type()
                .map_err(|error| ManageError::io("inspect", &entry.path(), error))?
                .is_dir();
            if !is_directory {
                continue;
            }
            if let Some(name) = entry.file_name().to_str() {
                versions.push(name.to_string());
            }
        }
        sort_newest_first(&mut versions);

        for version in versions {
            let root = toolchain_root(toolchains_root, channel, &version);
            let is_current = current
                .as_ref()
                .is_some_and(|selected| selected.channel == channel && selected.version == version);
            let is_complete = primary_binary_path(&root).is_file();
            installed.push(InstalledToolchain {
                channel,
                version,
                root,
                is_current,
                is_complete,
            });
        }
    }

    Ok(installed)
}

/// What a selection produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selected {
    /// The channel now selected.
    pub channel: Channel,
    /// The version now selected.
    pub version: String,
    /// The toolchain root the launcher will dispatch into.
    pub root: PathBuf,
    /// Whether this was already the selected toolchain.
    pub was_already_current: bool,
}

/// Selects an installed toolchain by rewriting `current.toml`.
///
/// Refuses a version that is not installed, and one whose tree has lost the
/// binary the launcher runs: selecting either would leave `kira` unable to
/// dispatch, which is a failure to report now rather than later.
pub fn select(
    toolchains_root: &Path,
    channel: Channel,
    version: &str,
) -> Result<Selected, ManageError> {
    let root = installed_root(toolchains_root, channel, version)?;

    let primary = primary_binary_path(&root);
    if !primary.is_file() {
        return Err(ManageError::Incomplete {
            channel: channel.dir_name(),
            version: version.to_string(),
            expected: primary,
        });
    }

    let was_already_current = read_current(toolchains_root)?
        .is_some_and(|current| current.channel == channel && current.version == version);

    write_current(
        toolchains_root,
        &CurrentToolchain {
            channel,
            version: version.to_string(),
            primary: PRIMARY_BINARY.to_string(),
        },
    )?;

    Ok(Selected {
        channel,
        version: version.to_string(),
        root,
        was_already_current,
    })
}

/// What an uninstall removed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Uninstalled {
    /// The channel it was removed from.
    pub channel: Channel,
    /// The version that was removed.
    pub version: String,
    /// The directory that was removed.
    pub root: PathBuf,
    /// Whether it was the selected toolchain, so nothing is selected now.
    pub was_current: bool,
}

/// Removes one installed toolchain.
///
/// Removes exactly `<channel>/<version>/`. When the removed version was the
/// selected one, `current.toml` is deleted rather than repointed: guessing a
/// replacement would silently change which compiler a user runs.
pub fn uninstall(
    toolchains_root: &Path,
    channel: Channel,
    version: &str,
) -> Result<Uninstalled, ManageError> {
    let root = installed_root(toolchains_root, channel, version)?;

    let was_current = read_current(toolchains_root)?
        .is_some_and(|current| current.channel == channel && current.version == version);

    std::fs::remove_dir_all(&root).map_err(|error| ManageError::io("remove", &root, error))?;

    if was_current {
        let path = current_toolchain_path(toolchains_root);
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(ManageError::io("remove", &path, error)),
        }
    }

    Ok(Uninstalled {
        channel,
        version: version.to_string(),
        root,
        was_current,
    })
}

/// The root of an installed toolchain, or a typed refusal.
fn installed_root(
    toolchains_root: &Path,
    channel: Channel,
    version: &str,
) -> Result<PathBuf, ManageError> {
    if !is_single_component(version) {
        return Err(ManageError::InvalidVersion {
            version: version.to_string(),
        });
    }
    let root = toolchain_root(toolchains_root, channel, version);
    if !root.is_dir() {
        return Err(ManageError::NotInstalled {
            channel: channel.dir_name(),
            version: version.to_string(),
            expected: root,
        });
    }
    Ok(root)
}

/// `<root>/bin/<primary>` — the binary the launcher dispatches to.
fn primary_binary_path(root: &Path) -> PathBuf {
    root.join("bin").join(executable_name(PRIMARY_BINARY))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_a_version_and_refuses_anything_that_could_escape_its_channel() {
        for good in ["1.7.3", "2026.07.2", "1.7.0-rc1"] {
            assert!(is_single_component(good), "`{good}` is a version name");
        }
        for bad in ["", ".", "..", "../llvm", "a/b", "/etc", "1.7.3/"] {
            assert!(!is_single_component(bad), "`{bad}` must be refused");
        }
    }

    #[test]
    fn refuses_a_traversing_version_before_touching_the_filesystem() {
        let root = std::env::temp_dir().join(format!("knvm_traverse_{}", std::process::id()));
        let error = uninstall(&root, Channel::Release, "../llvm")
            .expect_err("a traversing version must never be joined");
        assert!(matches!(error, ManageError::InvalidVersion { .. }));
        assert!(!root.exists(), "the refusal must not create anything");
    }

    #[test]
    fn lists_nothing_for_a_root_that_was_never_installed_into() {
        let root = std::env::temp_dir().join(format!("knvm_emptylist_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        assert_eq!(list(&root).expect("an absent root is not an error"), &[]);
    }

    #[test]
    fn refuses_to_select_a_version_that_is_not_installed() {
        let root = std::env::temp_dir().join(format!("knvm_noselect_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let error =
            select(&root, Channel::Release, "1.7.3").expect_err("nothing is installed here");
        assert!(matches!(error, ManageError::NotInstalled { .. }));
        assert!(
            !current_toolchain_path(&root).exists(),
            "a refused selection must write no current.toml"
        );
    }
}
