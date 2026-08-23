//! Unpacking a published archive.
//!
//! Shared by everything knvm installs, because the question "which tool on this
//! machine reads a zip" has one answer and no caller should have to know it
//! twice. The errors are archive-shaped rather than product-shaped: each caller
//! maps them into whatever it is installing.

use std::path::{Path, PathBuf};

use crate::install::InstallError;

/// Why an archive could not be unpacked.
#[derive(Debug, thiserror::Error)]
pub enum UnpackError {
    /// The archive format is not one this can unpack.
    #[error("`{format}` is not an archive format knvm unpacks (expected `tar.xz`, `tar.gz` or `zip`)")]
    UnsupportedFormat {
        /// What the caller asked for.
        format: String,
    },
    /// No unpacking tool for the format is on this host.
    #[error(
        "none of {} were found on PATH; knvm unpacks `{format}` archives with one of them",
        .tools.join(", ")
    )]
    UnpackerUnavailable {
        /// The tools that were tried, in order.
        tools: &'static [&'static str],
        /// The format they would have unpacked.
        format: String,
    },
    /// A tool ran and refused the archive.
    #[error("could not unpack `{}`: {detail}", .archive.display())]
    Failed {
        /// The archive being unpacked.
        archive: PathBuf,
        /// What the tool reported.
        detail: String,
    },
    /// The filesystem refused something on the way.
    #[error(transparent)]
    Io(#[from] InstallError),
}

/// The tools that can unpack `format`, in the order they are tried.
///
/// `tar` reads `.tar.xz` and `.tar.gz` without being told the compression, and
/// unpacks it on every Windows shell. `unzip` is the format's own tool and is
/// what a git-for-Windows or MSYS environment has; a stock `cmd` or PowerShell
/// has none, but does have a `tar` that is bsdtar and reads zip. The `tar` on
/// those same MSYS shells is GNU tar, which refuses a zip outright — so the
/// fallthrough in `extract` is on failure and not only on a missing tool, or
/// the first environment would never reach the second's tool.
pub(crate) fn unpackers(format: &str) -> Option<&'static [&'static str]> {
    match format {
        "tar.xz" | "tar.gz" => Some(&["tar"]),
        "zip" => Some(&["unzip", "tar"]),
        _ => None,
    }
}

/// How `tool` is asked to unpack `archive` into `destination`.
pub(crate) fn unpack_arguments<'a>(
    tool: &str,
    archive: &'a Path,
    destination: &'a Path,
) -> Vec<&'a std::ffi::OsStr> {
    match tool {
        "unzip" => vec![
            "-q".as_ref(),
            archive.as_os_str(),
            "-d".as_ref(),
            destination.as_os_str(),
        ],
        _ => vec![
            "-xf".as_ref(),
            archive.as_os_str(),
            "-C".as_ref(),
            destination.as_os_str(),
        ],
    }
}

/// Unpacks an archive into `destination`.
///
/// Each tool [`unpackers`] names is tried until one succeeds; a tool that is
/// absent or that refuses the archive hands over to the next, and the last
/// refusal is what gets reported when none of them worked.
pub(crate) fn extract(
    archive: &Path,
    destination: &Path,
    format: &str,
) -> Result<(), UnpackError> {
    let Some(tools) = unpackers(format) else {
        return Err(UnpackError::UnsupportedFormat {
            format: format.to_string(),
        });
    };
    std::fs::create_dir_all(destination)
        .map_err(|error| InstallError::io("create", destination, error))?;

    let mut refused: Option<String> = None;
    for tool in tools {
        let arguments = unpack_arguments(tool, archive, destination);
        let output = match std::process::Command::new(tool).args(&arguments).output() {
            Ok(output) => output,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(InstallError::io("run the unpacker on", archive, error).into());
            }
        };
        if output.status.success() {
            return Ok(());
        }
        refused = Some(format!(
            "{tool} exited with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }

    match refused {
        Some(detail) => Err(UnpackError::Failed {
            archive: archive.to_path_buf(),
            detail,
        }),
        None => Err(UnpackError::UnpackerUnavailable {
            tools,
            format: format.to_string(),
        }),
    }
}
