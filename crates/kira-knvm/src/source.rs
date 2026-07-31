//! Where a toolchain archive comes from, and the one offline implementation.
//!
//! Everything downstream of [`ReleaseSource`] — extract, validate, move into
//! place, write `current.toml` — is identical for every implementation, so a
//! test driving [`DirectoryReleaseSource`] exercises the shipped install path
//! minus only the transport.

use std::cmp::Ordering;
use std::path::{Path, PathBuf};

use kira_toolchain::Channel;

use crate::digest::{Sha256, checksum_file_name, parse_checksum_file};

/// Why a release could not be listed or fetched.
#[derive(Debug, thiserror::Error)]
pub enum ReleaseSourceError {
    /// The channel exists but publishes no version.
    #[error("no versions published on the `{channel}` channel")]
    ChannelEmpty {
        /// The channel directory name.
        channel: &'static str,
    },
    /// A specific version was asked for and the source does not have it.
    #[error("version `{version}` was not found on the `{channel}` channel")]
    VersionNotFound {
        /// The channel directory name.
        channel: &'static str,
        /// The version that was asked for.
        version: String,
    },
    /// The version exists but ships no artifact for this host.
    #[error("release `{version}` has no artifact for this host (expected `{artifact}`)")]
    MissingHostArtifact {
        /// The version that was resolved.
        version: String,
        /// The artifact file name that was looked for.
        artifact: String,
    },
    /// This host is not one Kira publishes artifacts for.
    #[error("unsupported host `{os}`/`{arch}`: no Kira release artifacts are published for it")]
    UnsupportedHost {
        /// `std::env::consts::OS`.
        os: &'static str,
        /// `std::env::consts::ARCH`.
        arch: &'static str,
    },
    /// The transport itself failed (process, network, or HTTP status).
    #[error("could not reach the release source: {detail}")]
    TransportFailed {
        /// What went wrong, already rendered.
        detail: String,
    },
    /// `curl` is required to talk to a remote source and is not on this host.
    #[error(
        "`curl` was not found on PATH; knvm downloads releases with it. \
         Install curl, or install the toolchain from a local directory instead"
    )]
    CurlUnavailable,
    /// The release feed parsed as JSON but is not the shape the API documents.
    #[error("the release feed could not be understood: {detail}")]
    MalformedFeed {
        /// What was wrong with the feed.
        detail: String,
    },
    /// A checksum sidecar was published but carries no readable digest.
    ///
    /// Refused rather than treated as absent: a sidecar that exists is a claim
    /// about the artifact, and one that cannot be read is a publishing failure
    /// to report, not a verification to skip.
    #[error(
        "the published checksum for `{artifact}` is not a SHA-256 digest \
         (found `{found}`)"
    )]
    MalformedChecksum {
        /// The artifact the sidecar belongs to.
        artifact: String,
        /// What the sidecar held instead, truncated to one line.
        found: String,
    },
    /// A filesystem operation against the source failed.
    #[error("{operation} `{path}`: {source}")]
    Io {
        /// What was being attempted.
        operation: &'static str,
        /// The path it was attempted on.
        path: PathBuf,
        /// The underlying error.
        source: std::io::Error,
    },
}

impl ReleaseSourceError {
    /// An [`Io`](Self::Io) error carrying the path it happened on.
    pub(crate) fn io(operation: &'static str, path: &Path, source: std::io::Error) -> Self {
        Self::Io {
            operation,
            path: path.to_path_buf(),
            source,
        }
    }
}

/// Somewhere toolchain archives can be listed and fetched from.
///
/// Consumed as `&dyn ReleaseSource` so the install path stays a single
/// non-generic function at the crate boundary.
pub trait ReleaseSource {
    /// Every version published on `channel`, newest first.
    fn available_versions(&self, channel: Channel) -> Result<Vec<String>, ReleaseSourceError>;

    /// Materializes the archive for `version` inside `dest_dir` and returns the
    /// path it was written to.
    fn fetch_archive(
        &self,
        channel: Channel,
        version: &str,
        dest_dir: &Path,
    ) -> Result<PathBuf, ReleaseSourceError>;

    /// The digest this source publishes for `version`'s host artifact.
    ///
    /// `Ok(None)` means the source publishes no digest for it, which installs
    /// as unverified. There is deliberately no default implementation: a source
    /// added later must state its answer rather than inherit "unverified" from
    /// a trait definition nobody looked at.
    fn published_checksum(
        &self,
        channel: Channel,
        version: &str,
    ) -> Result<Option<Sha256>, ReleaseSourceError>;
}

/// Reads a checksum sidecar's text into a digest, naming the artifact when it
/// cannot be read.
///
/// Shared by every source: the sidecar format is the feed's contract, not one
/// transport's.
pub(crate) fn read_published_checksum(
    artifact: &str,
    contents: &str,
) -> Result<Sha256, ReleaseSourceError> {
    parse_checksum_file(contents).ok_or_else(|| ReleaseSourceError::MalformedChecksum {
        artifact: artifact.to_string(),
        found: contents.lines().next().unwrap_or("").trim().to_string(),
    })
}

/// The artifact file name a release publishes for one host.
#[must_use]
pub fn archive_file_name(version: &str, host_key: &str) -> String {
    format!("kira-{version}-{host_key}.tar.gz")
}

/// The release-artifact key of the running host.
///
/// Reuses the managed-LLVM bundle key set so a host is spelled exactly one way
/// across the toolchain tree.
pub fn current_host_key() -> Result<&'static str, ReleaseSourceError> {
    kira_toolchain::llvm_layout::current_host_llvm_bundle_key().ok_or(
        ReleaseSourceError::UnsupportedHost {
            os: std::env::consts::OS,
            arch: std::env::consts::ARCH,
        },
    )
}

/// Orders two version strings numerically where they are numeric.
///
/// `2026.07.10` sorts above `2026.07.2`, which a lexicographic sort gets wrong.
/// Components that are not numbers fall back to a string comparison, and a
/// version with more components sorts above its own prefix (`1.7.3` above
/// `1.7`).
///
/// This is deliberately not semver: a trailing prerelease tag is compared as
/// text, so `1.7.0-rc1` sorts *above* `1.7.0` rather than below it. Kira
/// separates prereleases by channel instead of by tag, so a `-rc` version on
/// the release channel is out of contract; ordering it correctly is a semver
/// parser's job and belongs here only once such tags are actually published.
#[must_use]
pub fn compare_versions(left: &str, right: &str) -> Ordering {
    let mut left_parts = left.split('.');
    let mut right_parts = right.split('.');
    loop {
        match (left_parts.next(), right_parts.next()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(left_part), Some(right_part)) => {
                let ordering = match (left_part.parse::<u64>(), right_part.parse::<u64>()) {
                    (Ok(left_number), Ok(right_number)) => left_number.cmp(&right_number),
                    _ => left_part.cmp(right_part),
                };
                if ordering != Ordering::Equal {
                    return ordering;
                }
            }
        }
    }
}

/// Sorts versions newest first, in place.
pub fn sort_newest_first(versions: &mut [String]) {
    versions.sort_by(|left, right| compare_versions(right, left));
}

/// A release source rooted at a local directory.
///
/// The directory is laid out exactly as a published feed is:
/// `<root>/<channel>/<version>/kira-<version>-<host-key>.tar.gz`. This is the
/// implementation the tests drive end to end, and the same one an offline
/// install from a mirrored directory will use.
#[derive(Debug, Clone)]
pub struct DirectoryReleaseSource {
    root: PathBuf,
    host_key: String,
}

impl DirectoryReleaseSource {
    /// A source rooted at `root`, publishing artifacts for the running host.
    pub fn new(root: impl Into<PathBuf>) -> Result<Self, ReleaseSourceError> {
        Ok(Self::with_host_key(root, current_host_key()?))
    }

    /// A source rooted at `root`, publishing artifacts for a named host.
    #[must_use]
    pub fn with_host_key(root: impl Into<PathBuf>, host_key: impl Into<String>) -> Self {
        Self {
            root: root.into(),
            host_key: host_key.into(),
        }
    }

    /// The directory holding one channel's versions.
    fn channel_dir(&self, channel: Channel) -> PathBuf {
        self.root.join(channel.dir_name())
    }
}

impl ReleaseSource for DirectoryReleaseSource {
    fn available_versions(&self, channel: Channel) -> Result<Vec<String>, ReleaseSourceError> {
        let directory = self.channel_dir(channel);
        let entries = match std::fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(ReleaseSourceError::ChannelEmpty {
                    channel: channel.dir_name(),
                });
            }
            Err(error) => return Err(ReleaseSourceError::io("read", &directory, error)),
        };

        let mut versions = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|error| ReleaseSourceError::io("read", &directory, error))?;
            let is_directory = entry
                .file_type()
                .map_err(|error| ReleaseSourceError::io("inspect", &entry.path(), error))?
                .is_dir();
            if !is_directory {
                continue;
            }
            if let Some(name) = entry.file_name().to_str() {
                versions.push(name.to_string());
            }
        }
        sort_newest_first(&mut versions);
        Ok(versions)
    }

    fn fetch_archive(
        &self,
        channel: Channel,
        version: &str,
        dest_dir: &Path,
    ) -> Result<PathBuf, ReleaseSourceError> {
        let version_dir = self.channel_dir(channel).join(version);
        if !version_dir.is_dir() {
            return Err(ReleaseSourceError::VersionNotFound {
                channel: channel.dir_name(),
                version: version.to_string(),
            });
        }

        let file_name = archive_file_name(version, &self.host_key);
        let archive = version_dir.join(&file_name);
        if !archive.is_file() {
            return Err(ReleaseSourceError::MissingHostArtifact {
                version: version.to_string(),
                artifact: file_name,
            });
        }

        std::fs::create_dir_all(dest_dir)
            .map_err(|error| ReleaseSourceError::io("create", dest_dir, error))?;
        let destination = dest_dir.join(&file_name);
        std::fs::copy(&archive, &destination)
            .map_err(|error| ReleaseSourceError::io("copy", &archive, error))?;
        Ok(destination)
    }

    fn published_checksum(
        &self,
        channel: Channel,
        version: &str,
    ) -> Result<Option<Sha256>, ReleaseSourceError> {
        let file_name = archive_file_name(version, &self.host_key);
        let sidecar = self
            .channel_dir(channel)
            .join(version)
            .join(checksum_file_name(&file_name));
        let contents = match std::fs::read_to_string(&sidecar) {
            Ok(contents) => contents,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(ReleaseSourceError::io("read", &sidecar, error)),
        };
        read_published_checksum(&file_name, &contents).map(Some)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn orders_versions_numerically_not_lexicographically() {
        assert_eq!(
            compare_versions("2026.07.10", "2026.07.2"),
            Ordering::Greater
        );
        assert_eq!(compare_versions("1.7.3", "1.10.0"), Ordering::Less);
        assert_eq!(compare_versions("1.7.3", "1.7.3"), Ordering::Equal);
        assert_eq!(compare_versions("1.7.3", "1.7"), Ordering::Greater);
        assert_eq!(compare_versions("1.7.0-rc1", "1.7.0"), Ordering::Greater);
    }

    #[test]
    fn sorts_newest_first() {
        let mut versions = vec![
            "1.7.3".to_string(),
            "1.10.0".to_string(),
            "1.9.12".to_string(),
        ];
        sort_newest_first(&mut versions);
        assert_eq!(versions, ["1.10.0", "1.9.12", "1.7.3"]);
    }

    #[test]
    fn names_the_host_artifact() {
        assert_eq!(
            archive_file_name("1.7.3", "aarch64-macos"),
            "kira-1.7.3-aarch64-macos.tar.gz"
        );
    }

    #[test]
    fn reports_an_absent_channel_as_empty_rather_than_as_an_io_failure() {
        let source = DirectoryReleaseSource::with_host_key(
            std::env::temp_dir().join("knvm-nonexistent-feed-root"),
            "aarch64-macos",
        );
        let error = source
            .available_versions(Channel::Release)
            .expect_err("an absent feed root has no versions");
        assert!(matches!(
            error,
            ReleaseSourceError::ChannelEmpty { channel: "release" }
        ));
    }
}
