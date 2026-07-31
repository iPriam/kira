//! Replacing the tools with a published build: `knvm self-update`.
//!
//! `sinstall` builds the tools from a checkout; this fetches them from a
//! release, for the far more common machine that has no checkout. Both land
//! the same three binaries in `<kira-home>/bin` by the same stage-then-rename,
//! which is what makes replacing the running `knvm` safe on unix.
//!
//! # What it updates, and what it deliberately does not
//!
//! The *tools* — `knvm`, the `kira` launcher, and the launcher's
//! `kira-language-server` alias — and nothing else. Installed toolchains are
//! untouched and `current.toml` is not rewritten: the tools select between
//! toolchain versions, so updating them must not silently move a user to a
//! different compiler. `knvm install latest` is the verb that does that, and
//! it stays a separate decision.

use std::path::{Path, PathBuf};

use kira_toolchain::{Channel, LANGUAGE_SERVER_BINARY, executable_name};

use crate::digest::{Sha256, checksum_file_name};
use crate::github::{self, GitHubReleaseSource, releases_on_channel};
use crate::install::{InstallError, Staging};
use crate::source::{ReleaseSourceError, read_published_checksum};

/// The tools archive a release publishes for one host.
///
/// The name is the contract `release.yml` packages to, exactly as
/// `archive_file_name` is for the toolchain archive.
#[must_use]
pub fn tools_archive_file_name(version: &str, host_key: &str) -> String {
    format!("knvm-{version}-{host_key}.tar.gz")
}

/// The tools an update installs, as `(name inside the archive, installed name)`.
///
/// The archive already holds them under the names they are installed as — it
/// is packed from the same list `sinstall` installs from — so this is a check
/// that every one of them arrived, not a rename table.
fn tool_names() -> [String; 3] {
    [
        executable_name("knvm"),
        executable_name("kira"),
        executable_name(LANGUAGE_SERVER_BINARY),
    ]
}

/// Why the tools could not be updated.
#[derive(Debug, thiserror::Error)]
pub enum SelfUpdateError {
    /// The release feed could not be read.
    #[error(transparent)]
    Source(#[from] ReleaseSourceError),
    /// The newest published version is the one already running.
    ///
    /// Not an error the process exits non-zero on; the binary reports it and
    /// succeeds, because "already current" is the good outcome of an update.
    #[error("`knvm` is already {version}, the newest on the `{channel}` channel")]
    AlreadyCurrent {
        /// The version that is both running and newest.
        version: String,
        /// The channel that was checked.
        channel: &'static str,
    },
    /// The release publishes no tools archive for this host.
    #[error("release `{version}` publishes no `{asset}`")]
    MissingAsset {
        /// The version that was resolved.
        version: String,
        /// The asset that was looked for.
        asset: String,
    },
    /// The downloaded archive is not the one the release published.
    #[error(
        "`{asset}` does not match the checksum published for it\n  \
         published: {expected}\n  \
         downloaded: {actual}\n\
         The download is corrupt or the archive was changed after publication; \
         the installed tools were not touched"
    )]
    ChecksumMismatch {
        /// The asset that was fetched.
        asset: String,
        /// The digest the release publishes.
        expected: Sha256,
        /// The digest of the bytes that arrived.
        actual: Sha256,
    },
    /// `tar` is required to unpack the archive and is not on this host.
    #[error("`tar` was not found on PATH; knvm unpacks archives with it")]
    TarUnavailable,
    /// `tar` ran and refused the archive.
    #[error("could not unpack `{}`: {detail}", .archive.display())]
    ExtractFailed {
        /// The archive that was being unpacked.
        archive: PathBuf,
        /// What `tar` reported.
        detail: String,
    },
    /// The unpacked archive is missing a tool it must carry.
    #[error(
        "the published tools archive has no `{tool}`; the installed tools were \
         not touched"
    )]
    MissingTool {
        /// The tool that was absent.
        tool: String,
    },
    /// A filesystem operation failed.
    #[error(transparent)]
    Io(#[from] InstallError),
}

/// What an update produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelfUpdated {
    /// The version now installed.
    pub version: String,
    /// The version that was running when the update started.
    pub previous_version: String,
    /// Where the tools were installed.
    pub bin_dir: PathBuf,
    /// The digest that was verified, when the release published one.
    pub verified: Option<Sha256>,
}

/// Fetches the newest published tools and replaces the installed ones.
///
/// `running_version` is what this binary reports itself as, which is what
/// decides whether there is anything to do. It is passed in rather than read
/// from the crate so a test can drive both outcomes.
pub fn self_update(
    kira_home: &Path,
    source: &GitHubReleaseSource,
    channel: Channel,
    running_version: &str,
) -> Result<SelfUpdated, SelfUpdateError> {
    let newest = source.newest_version(channel)?;
    if newest == running_version {
        return Err(SelfUpdateError::AlreadyCurrent {
            version: newest,
            channel: channel.dir_name(),
        });
    }

    let bin_dir = kira_home.join("bin");
    std::fs::create_dir_all(&bin_dir)
        .map_err(|error| InstallError::io("create", &bin_dir, error))?;

    // Staged under the kira home rather than the toolchains root: this touches
    // no toolchain, and a partly-downloaded tools archive has no business
    // sitting beside them.
    let staging = Staging::create(kira_home)?;
    let (archive, verified) = source.fetch_tools_archive(channel, &newest, staging.path())?;

    let unpacked = staging.path().join("unpacked");
    extract(&archive, &unpacked)?;
    let payload = locate_tools(&unpacked)?;

    // Every tool is checked present before any is replaced, so a truncated
    // archive cannot leave half the tools updated and half stale.
    let mut staged = Vec::new();
    for name in tool_names() {
        let built = payload.join("bin").join(&name);
        if !built.is_file() {
            return Err(SelfUpdateError::MissingTool { tool: name });
        }
        staged.push((name, built));
    }

    for (name, built) in staged {
        // Stage beside the destination, then rename over it: replacing the
        // running `knvm` this way is safe on unix, where writing onto a busy
        // binary is not.
        let incoming = bin_dir.join(format!(".incoming-{name}"));
        std::fs::copy(&built, &incoming)
            .map_err(|error| InstallError::io("copy the tool to", &incoming, error))?;
        copy_executable_bit(&built, &incoming)?;
        let destination = bin_dir.join(&name);
        std::fs::rename(&incoming, &destination)
            .map_err(|error| InstallError::io("move the tool into", &destination, error))?;
    }

    Ok(SelfUpdated {
        version: newest,
        previous_version: running_version.to_string(),
        bin_dir,
        verified,
    })
}

/// Gives the staged copy the source's permissions.
///
/// `std::fs::copy` already carries the mode across on unix; this restates it
/// where it matters, because a tool that arrives without its executable bit is
/// a tool the shell refuses to run.
fn copy_executable_bit(source: &Path, destination: &Path) -> Result<(), SelfUpdateError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(source)
            .map_err(|error| InstallError::io("inspect", source, error))?
            .permissions()
            .mode();
        std::fs::set_permissions(destination, std::fs::Permissions::from_mode(mode | 0o755))
            .map_err(|error| InstallError::io("set the mode of", destination, error))?;
    }
    #[cfg(not(unix))]
    {
        let _ = (source, destination);
    }
    Ok(())
}

/// Unpacks the tools archive.
fn extract(archive: &Path, destination: &Path) -> Result<(), SelfUpdateError> {
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
            return Err(SelfUpdateError::TarUnavailable);
        }
        Err(error) => return Err(InstallError::io("run tar on", archive, error).into()),
    };
    if !output.status.success() {
        return Err(SelfUpdateError::ExtractFailed {
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

/// Finds the `bin/`-bearing directory inside an unpacked tools archive.
fn locate_tools(unpacked: &Path) -> Result<PathBuf, SelfUpdateError> {
    if unpacked.join("bin").is_dir() {
        return Ok(unpacked.to_path_buf());
    }
    let entries =
        std::fs::read_dir(unpacked).map_err(|error| InstallError::io("read", unpacked, error))?;
    for entry in entries {
        let entry = entry.map_err(|error| InstallError::io("read", unpacked, error))?;
        let path = entry.path();
        if path.join("bin").is_dir() {
            return Ok(path);
        }
    }
    Err(SelfUpdateError::MissingTool {
        tool: executable_name("knvm"),
    })
}

impl GitHubReleaseSource {
    /// The newest version published on a channel.
    pub fn newest_version(&self, channel: Channel) -> Result<String, ReleaseSourceError> {
        use crate::source::ReleaseSource as _;
        self.available_versions(channel)?.into_iter().next().ok_or(
            ReleaseSourceError::ChannelEmpty {
                channel: channel.dir_name(),
            },
        )
    }

    /// Downloads a version's tools archive, verifying it when a sidecar exists.
    fn fetch_tools_archive(
        &self,
        channel: Channel,
        version: &str,
        into: &Path,
    ) -> Result<(PathBuf, Option<Sha256>), SelfUpdateError> {
        let release = self.release_on(channel, version)?;
        let asset = tools_archive_file_name(version, self.host_key());
        let url =
            github::asset_named(&release, &asset).ok_or_else(|| SelfUpdateError::MissingAsset {
                version: version.to_string(),
                asset: asset.clone(),
            })?;

        std::fs::create_dir_all(into).map_err(|error| InstallError::io("create", into, error))?;
        let archive = into.join(&asset);
        github::download(url, &archive)?;

        let sidecar = checksum_file_name(&asset);
        let verified = match github::asset_named(&release, &sidecar) {
            None => None,
            Some(sidecar_url) => {
                let expected = read_published_checksum(&asset, &github::get_text(sidecar_url)?)?;
                let actual = Sha256::of_file(&archive).map_err(|error| {
                    InstallError::io("read the downloaded archive", &archive, error)
                })?;
                if actual != expected {
                    return Err(SelfUpdateError::ChecksumMismatch {
                        asset,
                        expected,
                        actual,
                    });
                }
                Some(expected)
            }
        };

        Ok((archive, verified))
    }
}

/// Every version published on a channel, newest first, for `list --remote`.
///
/// Separate from `available_versions` only in that it does not treat an empty
/// channel as an error: a listing of nothing published yet is a listing, and
/// the caller renders it as such.
pub fn published_versions(
    source: &GitHubReleaseSource,
    channel: Channel,
) -> Result<Vec<String>, ReleaseSourceError> {
    Ok(releases_on_channel(&source.entries()?, channel)
        .into_iter()
        .map(|entry| entry.version)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_the_tools_archive_the_release_workflow_packages() {
        assert_eq!(
            tools_archive_file_name("1.7.3", "aarch64-macos"),
            "knvm-1.7.3-aarch64-macos.tar.gz"
        );
    }

    /// The three tools are the three `sinstall` installs. If one list grows a
    /// tool the other does not, a self-update silently leaves it stale.
    #[test]
    fn updates_exactly_the_tools_sinstall_installs() {
        let names = tool_names();
        assert_eq!(names.len(), 3);
        assert!(names.contains(&executable_name("knvm")));
        assert!(names.contains(&executable_name("kira")));
        assert!(names.contains(&executable_name(LANGUAGE_SERVER_BINARY)));
    }
}
