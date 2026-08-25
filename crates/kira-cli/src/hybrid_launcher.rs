//! Locating and staging the standalone hybrid launcher.
//!
//! A hybrid *bundle* is three files a session loads; a hybrid *program* is those
//! three plus an executable the operating system starts. The executable is not
//! compiled per program: one launcher binary — this workspace's
//! `kira-hybrid-launcher` — hosts any bundle, and a program build stages a copy
//! of it named after the program. That is what makes `.kira-build/<stem>` mean
//! the same thing after a hybrid build that it means after a native one:
//! something you can run.
//!
//! Beside this compiler, like every binary the toolchain ships: the launcher is
//! built by the same workspace as `kira`, and a build must never pick one up
//! from the PATH that came from a different Kira than the compiler that built
//! the bundle it will run.

use std::path::{Path, PathBuf};

/// Copies the launcher to `executable`, named for the program it will run.
///
/// The old file is removed first rather than overwritten: a running program
/// holds its image open in ways platforms disagree about, and a stale
/// executable that survives its own rebuild is exactly the confusion
/// `Artifacts::discard_runnable` exists to prevent.
pub fn stage(executable: &Path) -> Result<(), StageError> {
    let launcher = locate()?;
    match std::fs::remove_file(executable) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(source) => {
            return Err(StageError::Remove {
                path: executable.to_path_buf(),
                source,
            });
        }
    }
    std::fs::copy(&launcher, executable).map_err(|source| StageError::Copy {
        from: launcher,
        to: executable.to_path_buf(),
        source,
    })?;
    Ok(())
}

/// Where the launcher lives, when it can be found.
fn locate() -> Result<PathBuf, StageError> {
    let current = std::env::current_exe().map_err(StageError::LocateSelf)?;
    let directory = current.parent().unwrap_or_else(|| Path::new("."));
    let path = directory.join(executable_name("kira-hybrid-launcher"));
    if path.is_file() {
        return Ok(path);
    }
    Err(StageError::Missing(Box::new(LauncherMissing {
        searched: vec![path],
    })))
}

/// The platform's file name for an executable called `stem`.
fn executable_name(stem: &str) -> String {
    if cfg!(target_os = "windows") {
        format!("{stem}.exe")
    } else {
        stem.to_owned()
    }
}

/// Why a standalone executable could not be staged.
#[derive(Debug, thiserror::Error)]
pub enum StageError {
    /// This compiler's own image could not be located.
    #[error("cannot locate this executable: {0}")]
    LocateSelf(#[source] std::io::Error),
    /// No launcher ships beside this compiler.
    #[error(transparent)]
    Missing(#[from] Box<LauncherMissing>),
    /// A stale executable was still on disk and could not be cleared.
    #[error("cannot remove the previous executable `{path}`: {source}")]
    Remove {
        /// The stale file.
        path: PathBuf,
        /// What went wrong.
        #[source]
        source: std::io::Error,
    },
    /// The copy itself failed.
    #[error("cannot stage the executable as `{to}`: {source}")]
    Copy {
        /// Where the launcher was found.
        from: PathBuf,
        /// Where the staged copy goes.
        to: PathBuf,
        /// What went wrong.
        #[source]
        source: std::io::Error,
    },
}

/// Why no launcher could be found.
///
/// Carrying the search rather than a bare string, so the error names what was
/// looked for and says the one command that produces it — the same discipline a
/// missing cross runtime archive keeps.
#[derive(Debug, thiserror::Error)]
#[error(
    "no hybrid launcher was found beside this compiler\n\
     note: a standalone hybrid build stages `kira-hybrid-launcher` as the \
     program's executable, and it ships with the toolchain that built the bundle\n\
     note: build it with `cargo build -p kira-hybrid-launcher`, then leave it \
     beside the `kira` binary\n\
     note: looked in {searched}",
     searched = searched
        .iter()
        .map(|path| format!("`{}`", path.display()))
        .collect::<Vec<_>>()
        .join(", "),
)]
pub struct LauncherMissing {
    /// Every path that was looked in.
    pub searched: Vec<PathBuf>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_launcher_is_named_for_its_platform() {
        if cfg!(target_os = "windows") {
            assert_eq!(
                executable_name("kira-hybrid-launcher"),
                "kira-hybrid-launcher.exe"
            );
        } else {
            assert_eq!(
                executable_name("kira-hybrid-launcher"),
                "kira-hybrid-launcher"
            );
        }
    }

    /// The error is the remedy's vehicle: it must name the exact command that
    /// produces what is missing, the way a missing runtime archive does.
    #[test]
    fn a_missing_launcher_names_the_command_that_builds_one() {
        let error = LauncherMissing {
            searched: vec![PathBuf::from("/toolchain/bin/kira-hybrid-launcher")],
        };
        let text = error.to_string();
        assert!(
            text.contains("cargo build -p kira-hybrid-launcher"),
            "{text}"
        );
        assert!(
            text.contains("/toolchain/bin/kira-hybrid-launcher"),
            "{text}"
        );
    }
}
