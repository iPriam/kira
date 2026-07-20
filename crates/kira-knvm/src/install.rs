//! Installing a toolchain into `<toolchains-root>/<channel>/<version>/`.
//!
//! Every operation takes the toolchains root as an argument and never resolves
//! `HOME` itself: the binary resolves it once, and a test hands over a temp
//! directory without touching the process environment.
//!
//! # The install is atomic at the directory level
//!
//! Nothing is written under `<channel>/` until the archive has been extracted
//! and validated in a staging directory that belongs to this process alone. The
//! last step is a single `rename` of the validated tree into place, so a failed
//! or interrupted install leaves no half-toolchain a launcher could dispatch
//! to. Staging is removed on every exit path, success or failure.
//!
//! # What is guaranteed not to be touched
//!
//! `llvm/` and `libffi/` are version-independent siblings shared across
//! toolchain versions, keyed by their own versions. Installing a toolchain
//! writes exactly `<channel>/<version>/` and `current.toml`, and never those
//! subtrees.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use kira_toolchain::{Channel, CurrentToolchain, LANGUAGE_SERVER_BINARY, executable_name};

use crate::cli::VersionSpec;
use crate::source::{ReleaseSource, ReleaseSourceError};

/// The binary a toolchain's `current.toml` names as its primary.
pub const PRIMARY_BINARY: &str = "kirac";

/// The directory name of a package manifest inside the bundled Foundation.
const FOUNDATION_DIR_NAME: &str = "foundation";

/// The manifest that marks the bundled Foundation as a real Kira package.
const PACKAGE_MANIFEST_FILE_NAME: &str = "package.kira";

/// The directory installs are staged in before being moved into place.
const STAGING_DIR_NAME: &str = ".staging";

/// Why an install could not be completed.
#[derive(Debug, thiserror::Error)]
pub enum InstallError {
    /// The release could not be listed or fetched.
    #[error(transparent)]
    Source(#[from] ReleaseSourceError),
    /// `tar` is required to unpack an archive and is not on this host.
    #[error("`tar` was not found on PATH; knvm unpacks release archives with it")]
    TarUnavailable,
    /// `tar` ran and refused the archive.
    #[error("could not unpack `{archive}`: {detail}")]
    ExtractFailed {
        /// The archive that was being unpacked.
        archive: PathBuf,
        /// What `tar` reported.
        detail: String,
    },
    /// The archive did not contain a single toolchain tree.
    #[error(
        "`{archive}` does not contain a toolchain: expected one directory holding \
         `bin/` and `{FOUNDATION_DIR_NAME}/`"
    )]
    UnrecognizedArchiveLayout {
        /// The archive that was being unpacked.
        archive: PathBuf,
    },
    /// The unpacked tree has no primary binary.
    #[error("the unpacked toolchain has no `{}`", .expected.display())]
    MissingPrimaryBinary {
        /// Where the binary was expected.
        expected: PathBuf,
    },
    /// The unpacked tree has a primary binary that cannot be run.
    #[error("the unpacked toolchain's `{}` is not executable", .path.display())]
    PrimaryNotExecutable {
        /// The binary that lacks an executable bit.
        path: PathBuf,
    },
    /// The unpacked tree ships no language server beside its primary binary.
    #[error(
        "the unpacked toolchain has no executable `{}` — a toolchain ships its \
         editor server so the two cannot drift apart",
        .expected.display()
    )]
    MissingLanguageServer {
        /// Where the language server was expected.
        expected: PathBuf,
    },
    /// The unpacked tree ships no Foundation beside its binaries.
    #[error(
        "the unpacked toolchain has no bundled Foundation package at `{}`",
        .expected.display()
    )]
    MissingFoundation {
        /// Where `foundation/package.kira` was expected.
        expected: PathBuf,
    },
    /// `current.toml` exists but does not parse.
    #[error(
        "`{}` is not a readable selection; run `knvm use <version>` to rewrite it",
        .path.display()
    )]
    MalformedCurrent {
        /// The file that could not be parsed.
        path: PathBuf,
    },
    /// The version names something other than one directory.
    #[error("`{version}` is not a version name: a version is a single directory component")]
    InvalidVersion {
        /// The version that was refused.
        version: String,
    },
    /// The destination is occupied by something that is not a usable toolchain.
    #[error(
        "`{}` already exists but is not a usable toolchain ({detail}); \
         run `knvm uninstall {version}` and install again",
        .root.display()
    )]
    IncompleteInstall {
        /// The occupied toolchain root.
        root: PathBuf,
        /// The version that was being installed.
        version: String,
        /// What was found to be wrong with it.
        detail: String,
    },
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

impl InstallError {
    /// An [`Io`](Self::Io) error carrying the path it happened on.
    pub(crate) fn io(operation: &'static str, path: &Path, source: std::io::Error) -> Self {
        Self::Io {
            operation,
            path: path.to_path_buf(),
            source,
        }
    }
}

/// What an install produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Installed {
    /// The channel the toolchain was installed from.
    pub channel: Channel,
    /// The version that was resolved and installed.
    pub version: String,
    /// The toolchain root: `<toolchains-root>/<channel>/<version>`.
    pub root: PathBuf,
    /// Whether the version was already present, so nothing was fetched.
    ///
    /// An already-installed version is still selected: `install` implies `use`.
    pub already_installed: bool,
}

/// `<toolchains-root>/<channel>/<version>` — an installed toolchain root.
///
/// The user-level equivalents in `kira-toolchain` derive their root from
/// `KIRA_HOME`; these take the root explicitly so an install can target a
/// directory that is not the user's.
#[must_use]
pub fn toolchain_root(toolchains_root: &Path, channel: Channel, version: &str) -> PathBuf {
    toolchains_root.join(channel.dir_name()).join(version)
}

/// `<toolchains-root>/current.toml`.
#[must_use]
pub fn current_toolchain_path(toolchains_root: &Path) -> PathBuf {
    toolchains_root.join("current.toml")
}

/// Reads the selected toolchain, if one is selected.
///
/// A missing `current.toml` means nothing is selected and is not an error. A
/// present but unparsable one is: reporting it as "nothing selected" would let
/// `list` and `use` quietly disagree with the launcher, which reads the same
/// file and refuses to dispatch on it.
pub fn read_current(toolchains_root: &Path) -> Result<Option<CurrentToolchain>, InstallError> {
    let path = current_toolchain_path(toolchains_root);
    let contents = match std::fs::read_to_string(&path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(InstallError::io("read", &path, error)),
    };
    match CurrentToolchain::parse_toml(&contents) {
        Ok(current) => Ok(Some(current)),
        Err(_) => Err(InstallError::MalformedCurrent { path }),
    }
}

/// Writes `current.toml`, selecting a toolchain for the launcher.
pub fn write_current(
    toolchains_root: &Path,
    current: &CurrentToolchain,
) -> Result<(), InstallError> {
    std::fs::create_dir_all(toolchains_root)
        .map_err(|error| InstallError::io("create", toolchains_root, error))?;
    let path = current_toolchain_path(toolchains_root);
    std::fs::write(&path, current.to_toml())
        .map_err(|error| InstallError::io("write", &path, error))
}

/// Installs a toolchain and selects it.
///
/// Resolving `latest` lists the channel; an exact version is fetched directly,
/// so an install of a known version costs one transfer rather than a listing
/// plus a transfer. A version that is already installed skips the fetch and is
/// selected as it stands — `install` always implies `use`.
pub fn install(
    toolchains_root: &Path,
    source: &dyn ReleaseSource,
    spec: &VersionSpec,
    channel: Channel,
) -> Result<Installed, InstallError> {
    let version = resolve_version(source, spec, channel)?;
    if !is_single_component(&version) {
        return Err(InstallError::InvalidVersion { version });
    }
    let destination = toolchain_root(toolchains_root, channel, &version);

    // `is_dir` alone would report success for a half-written or hand-damaged
    // tree and select it, handing the launcher a toolchain it cannot run.
    let already_installed = destination.is_dir();
    if already_installed {
        validate(&destination).map_err(|error| InstallError::IncompleteInstall {
            root: destination.clone(),
            version: version.clone(),
            detail: error.to_string(),
        })?;
    }
    if !already_installed {
        let staging = Staging::create(toolchains_root)?;
        let archive = source.fetch_archive(channel, &version, staging.path())?;
        let unpacked = staging.path().join("unpacked");
        extract(&archive, &unpacked)?;
        let payload = locate_payload(&unpacked, &archive)?;
        validate(&payload)?;

        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| InstallError::io("create", parent, error))?;
        }
        std::fs::rename(&payload, &destination).map_err(|error| {
            InstallError::io("move the unpacked toolchain into", &destination, error)
        })?;
    }

    write_current(
        toolchains_root,
        &CurrentToolchain {
            channel,
            version: version.clone(),
            primary: PRIMARY_BINARY.to_string(),
        },
    )?;

    Ok(Installed {
        channel,
        version,
        root: destination,
        already_installed,
    })
}

/// Turns a requested version into a concrete one.
fn resolve_version(
    source: &dyn ReleaseSource,
    spec: &VersionSpec,
    channel: Channel,
) -> Result<String, InstallError> {
    match spec {
        VersionSpec::Exact(version) => Ok(version.clone()),
        VersionSpec::Latest => {
            let versions = source.available_versions(channel)?;
            versions
                .into_iter()
                .next()
                .ok_or(ReleaseSourceError::ChannelEmpty {
                    channel: channel.dir_name(),
                })
                .map_err(InstallError::from)
        }
    }
}

/// Unpacks a `.tar.gz` into `destination` with the system `tar`.
fn extract(archive: &Path, destination: &Path) -> Result<(), InstallError> {
    std::fs::create_dir_all(destination)
        .map_err(|error| InstallError::io("create", destination, error))?;
    let output = match std::process::Command::new("tar")
        .arg("-xzf")
        .arg(archive)
        .arg("-C")
        .arg(destination)
        .output()
    {
        Ok(output) => output,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(InstallError::TarUnavailable);
        }
        Err(error) => return Err(InstallError::io("run tar on", archive, error)),
    };
    if !output.status.success() {
        return Err(InstallError::ExtractFailed {
            archive: archive.to_path_buf(),
            detail: format!(
                "tar exited with {}: {}",
                output.status,
                String::from_utf8_lossy(&output.stderr).trim()
            ),
        });
    }
    Ok(())
}

/// Finds the toolchain root inside an unpacked archive.
///
/// Accepts both an archive whose members sit at its top level and the usual
/// one that wraps them in a single `kira-<version>/` directory.
fn locate_payload(unpacked: &Path, archive: &Path) -> Result<PathBuf, InstallError> {
    if unpacked.join("bin").is_dir() {
        return Ok(unpacked.to_path_buf());
    }

    let mut directories = Vec::new();
    let entries =
        std::fs::read_dir(unpacked).map_err(|error| InstallError::io("read", unpacked, error))?;
    for entry in entries {
        let entry = entry.map_err(|error| InstallError::io("read", unpacked, error))?;
        if entry.path().is_dir() {
            directories.push(entry.path());
        }
    }

    match directories.as_slice() {
        [only] if only.join("bin").is_dir() => Ok(only.clone()),
        _ => Err(InstallError::UnrecognizedArchiveLayout {
            archive: archive.to_path_buf(),
        }),
    }
}

/// Refuses an unpacked tree that is not a usable toolchain.
///
/// The things checked are the things that make the tree work: the launcher
/// dispatches to `bin/kirac` and to `bin/kira-language-server`, and
/// `import Foundation` resolves to `foundation/` beside them. The language
/// server is a hard requirement on purpose — an install without one leaves an
/// editor silently running whatever stale server it finds elsewhere.
pub(crate) fn validate(payload: &Path) -> Result<(), InstallError> {
    let primary = payload.join("bin").join(executable_name(PRIMARY_BINARY));
    if !primary.is_file() {
        return Err(InstallError::MissingPrimaryBinary { expected: primary });
    }
    if !is_executable(&primary)? {
        return Err(InstallError::PrimaryNotExecutable { path: primary });
    }

    let server = payload
        .join("bin")
        .join(executable_name(LANGUAGE_SERVER_BINARY));
    if !server.is_file() || !is_executable(&server)? {
        return Err(InstallError::MissingLanguageServer { expected: server });
    }

    let manifest = payload
        .join(FOUNDATION_DIR_NAME)
        .join(PACKAGE_MANIFEST_FILE_NAME);
    if !manifest.is_file() {
        return Err(InstallError::MissingFoundation { expected: manifest });
    }
    Ok(())
}

/// Whether a version names exactly one directory, so joining it cannot escape.
///
/// Checked on the string rather than on `Path::components`, which normalizes a
/// trailing separator away and would accept `1.7.3/`. A version is a directory
/// name, so any separator at all is a refusal. Every operation that joins a
/// version onto the toolchains root runs this first — install included, since
/// a release source is not necessarily trusted to name its versions sanely.
pub(crate) fn is_single_component(version: &str) -> bool {
    !version.is_empty()
        && version != "."
        && version != ".."
        && !version.contains(['/', '\\'])
        && !version.contains('\0')
}

/// Whether a file carries an executable bit.
///
/// Windows has no such bit; there, being a file is the whole test.
fn is_executable(path: &Path) -> Result<bool, InstallError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let metadata =
            std::fs::metadata(path).map_err(|error| InstallError::io("inspect", path, error))?;
        Ok(metadata.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    {
        Ok(path.is_file())
    }
}

/// A staging directory owned by this process, removed when it goes out of scope.
///
/// The drop is what makes a failed install leave nothing behind: every early
/// return from [`install`] unwinds through it.
pub(crate) struct Staging {
    path: PathBuf,
}

impl Staging {
    /// Creates a staging directory unique to this process and call.
    pub(crate) fn create(toolchains_root: &Path) -> Result<Self, InstallError> {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = toolchains_root
            .join(STAGING_DIR_NAME)
            .join(format!("{}-{unique}", std::process::id()));
        std::fs::create_dir_all(&path).map_err(|error| InstallError::io("create", &path, error))?;
        Ok(Self { path })
    }

    /// The directory itself.
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for Staging {
    fn drop(&mut self) {
        // A staging directory that outlives its install is garbage, not state:
        // there is nothing to report and nothing to retry, so a failed removal
        // is deliberately ignored rather than masking the install's own error.
        let _ = std::fs::remove_dir_all(&self.path);
        // Then the `.staging` parent, which `remove_dir` declines to remove
        // while a concurrent install still holds a directory under it.
        if let Some(parent) = self.path.parent() {
            let _ = std::fs::remove_dir(parent);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derives_the_managed_layout_from_an_explicit_root() {
        let root = Path::new("/tmp/knvm-root");
        assert_eq!(
            toolchain_root(root, Channel::Dev, "2026.07.2"),
            Path::new("/tmp/knvm-root/dev/2026.07.2")
        );
        assert_eq!(
            current_toolchain_path(root),
            Path::new("/tmp/knvm-root/current.toml")
        );
    }

    #[test]
    fn removes_its_staging_directory_when_dropped() {
        let root = std::env::temp_dir().join(format!("knvm_staging_{}", std::process::id()));
        let staged = {
            let staging = Staging::create(&root).expect("create staging");
            let path = staging.path().to_path_buf();
            assert!(path.is_dir());
            path
        };
        assert!(!staged.exists(), "staging must not outlive its install");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn treats_an_absent_current_toml_as_nothing_selected() {
        let root = std::env::temp_dir().join(format!("knvm_nocurrent_{}", std::process::id()));
        assert_eq!(read_current(&root).expect("absent is not an error"), None);
    }
}
