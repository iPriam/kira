//! The default release source: GitHub releases of `kira-lang-com/kira`.
//!
//! # Why a `curl` subprocess and not an HTTP crate
//!
//! Adding an HTTP client would pull a TLS stack and its transitive tree into a
//! workspace whose external dependencies are deliberately frozen — for the one
//! function in this crate that talks to a network. `curl` ships with macOS, is
//! present on every mainstream Linux, and has been in Windows since 1803, so
//! the subprocess is the smaller commitment. Its absence is a typed error
//! rather than a panic, and swapping in a native client later touches only
//! [`transport`] — nothing above it knows how bytes arrive.
//!
//! # What is testable here
//!
//! [`parse_release_feed`] and [`select_asset`] are pure functions over text,
//! unit-tested against canned API responses. No test in this crate opens a
//! network connection or runs `curl`.

use std::path::{Path, PathBuf};
use std::process::Command;

use kira_toolchain::Channel;

use crate::source::{ReleaseSource, ReleaseSourceError, archive_file_name, compare_versions};

/// The repository releases are published from.
pub const DEFAULT_REPOSITORY: &str = "kira-lang-com/kira";

/// How long a single transfer may take, in seconds.
const TRANSFER_TIMEOUT_SECONDS: &str = "120";

/// One published release, reduced to what an install needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseEntry {
    /// The version, with the tag's leading `v` stripped.
    pub version: String,
    /// The channel the release belongs to: prereleases are `dev`.
    pub channel: Channel,
    /// The downloadable assets attached to the release.
    pub assets: Vec<ReleaseAsset>,
}

/// One downloadable file attached to a release.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseAsset {
    /// The asset's file name.
    pub name: String,
    /// The URL the asset's bytes are served from.
    pub url: String,
}

/// Parses a GitHub `/releases` API response into release entries.
///
/// Draft releases are skipped: they are not visible to users. A release whose
/// tag or asset list is not the documented shape fails the whole parse rather
/// than being silently dropped — a feed that changed shape must not look like
/// a feed with fewer releases.
pub fn parse_release_feed(json: &str) -> Result<Vec<ReleaseEntry>, ReleaseSourceError> {
    let value: serde_json::Value =
        serde_json::from_str(json).map_err(|error| ReleaseSourceError::MalformedFeed {
            detail: format!("response is not JSON: {error}"),
        })?;
    let releases = value
        .as_array()
        .ok_or_else(|| ReleaseSourceError::MalformedFeed {
            detail: "expected a JSON array of releases".to_string(),
        })?;

    let mut entries = Vec::new();
    for release in releases {
        if release.get("draft").and_then(serde_json::Value::as_bool) == Some(true) {
            continue;
        }
        let tag = release
            .get("tag_name")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| ReleaseSourceError::MalformedFeed {
                detail: "a release has no string `tag_name`".to_string(),
            })?;
        let prerelease = release
            .get("prerelease")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);

        let mut assets = Vec::new();
        if let Some(listed) = release.get("assets") {
            let listed = listed
                .as_array()
                .ok_or_else(|| ReleaseSourceError::MalformedFeed {
                    detail: format!("release `{tag}` has a non-array `assets`"),
                })?;
            for asset in listed {
                let name = asset.get("name").and_then(serde_json::Value::as_str);
                let url = asset
                    .get("browser_download_url")
                    .and_then(serde_json::Value::as_str);
                let (Some(name), Some(url)) = (name, url) else {
                    return Err(ReleaseSourceError::MalformedFeed {
                        detail: format!(
                            "release `{tag}` has an asset without a `name` and \
                             `browser_download_url`"
                        ),
                    });
                };
                assets.push(ReleaseAsset {
                    name: name.to_string(),
                    url: url.to_string(),
                });
            }
        }

        entries.push(ReleaseEntry {
            version: strip_tag_prefix(tag).to_string(),
            channel: if prerelease {
                Channel::Dev
            } else {
                Channel::Release
            },
            assets,
        });
    }
    Ok(entries)
}

/// The version a release tag names: `v1.7.3` and `1.7.3` both mean `1.7.3`.
#[must_use]
pub fn strip_tag_prefix(tag: &str) -> &str {
    tag.strip_prefix('v').unwrap_or(tag)
}

/// The download URL of the asset this host installs from.
pub fn select_asset<'a>(
    release: &'a ReleaseEntry,
    host_key: &str,
) -> Result<&'a str, ReleaseSourceError> {
    let wanted = archive_file_name(&release.version, host_key);
    release
        .assets
        .iter()
        .find(|asset| asset.name == wanted)
        .map(|asset| asset.url.as_str())
        .ok_or(ReleaseSourceError::MissingHostArtifact {
            version: release.version.clone(),
            artifact: wanted,
        })
}

/// The releases of one channel, newest first.
#[must_use]
pub fn releases_on_channel(entries: &[ReleaseEntry], channel: Channel) -> Vec<ReleaseEntry> {
    let mut matching: Vec<ReleaseEntry> = entries
        .iter()
        .filter(|entry| entry.channel == channel)
        .cloned()
        .collect();
    matching.sort_by(|left, right| compare_versions(&right.version, &left.version));
    matching
}

/// Releases published on GitHub.
#[derive(Debug, Clone)]
pub struct GitHubReleaseSource {
    repository: String,
    host_key: String,
}

impl GitHubReleaseSource {
    /// The default source: `kira-lang-com/kira`, for the running host.
    pub fn for_host() -> Result<Self, ReleaseSourceError> {
        Ok(Self::new(
            DEFAULT_REPOSITORY,
            crate::source::current_host_key()?,
        ))
    }

    /// A source for a named repository and host.
    #[must_use]
    pub fn new(repository: impl Into<String>, host_key: impl Into<String>) -> Self {
        Self {
            repository: repository.into(),
            host_key: host_key.into(),
        }
    }

    /// The API URL the release feed is read from.
    #[must_use]
    pub fn feed_url(&self) -> String {
        format!(
            "https://api.github.com/repos/{}/releases?per_page=100",
            self.repository
        )
    }

    /// The whole feed, parsed.
    fn feed(&self) -> Result<Vec<ReleaseEntry>, ReleaseSourceError> {
        parse_release_feed(&transport::get_text(&self.feed_url())?)
    }
}

impl ReleaseSource for GitHubReleaseSource {
    fn available_versions(&self, channel: Channel) -> Result<Vec<String>, ReleaseSourceError> {
        let versions: Vec<String> = releases_on_channel(&self.feed()?, channel)
            .into_iter()
            .map(|entry| entry.version)
            .collect();
        if versions.is_empty() {
            return Err(ReleaseSourceError::ChannelEmpty {
                channel: channel.dir_name(),
            });
        }
        Ok(versions)
    }

    fn fetch_archive(
        &self,
        channel: Channel,
        version: &str,
        dest_dir: &Path,
    ) -> Result<PathBuf, ReleaseSourceError> {
        let release = releases_on_channel(&self.feed()?, channel)
            .into_iter()
            .find(|entry| entry.version == version)
            .ok_or_else(|| ReleaseSourceError::VersionNotFound {
                channel: channel.dir_name(),
                version: version.to_string(),
            })?;
        let url = select_asset(&release, &self.host_key)?;

        std::fs::create_dir_all(dest_dir)
            .map_err(|error| ReleaseSourceError::io("create", dest_dir, error))?;
        let destination = dest_dir.join(archive_file_name(version, &self.host_key));
        transport::download(url, &destination)?;
        Ok(destination)
    }
}

/// The only code in this crate that talks to a network.
///
/// Isolated so that replacing `curl` with a native client is a change to this
/// module alone, and so that everything above it is reachable by tests.
mod transport {
    use super::{Command, Path, ReleaseSourceError, TRANSFER_TIMEOUT_SECONDS};

    /// Fetches a URL as text.
    pub(super) fn get_text(url: &str) -> Result<String, ReleaseSourceError> {
        let output = run(&[
            "-fsSL",
            "--max-time",
            TRANSFER_TIMEOUT_SECONDS,
            "-H",
            "User-Agent: knvm",
            "-H",
            "Accept: application/vnd.github+json",
            url,
        ])?;
        String::from_utf8(output).map_err(|_| ReleaseSourceError::MalformedFeed {
            detail: "response was not valid UTF-8".to_string(),
        })
    }

    /// Downloads a URL to a file.
    pub(super) fn download(url: &str, destination: &Path) -> Result<(), ReleaseSourceError> {
        let Some(destination_text) = destination.to_str() else {
            return Err(ReleaseSourceError::TransportFailed {
                detail: format!(
                    "download path `{}` is not valid UTF-8",
                    destination.display()
                ),
            });
        };
        run(&[
            "-fsSL",
            "--max-time",
            TRANSFER_TIMEOUT_SECONDS,
            "-H",
            "User-Agent: knvm",
            "-o",
            destination_text,
            url,
        ])
        .map(|_| ())
    }

    /// Runs `curl` and returns its stdout, mapping every failure to a typed error.
    fn run(arguments: &[&str]) -> Result<Vec<u8>, ReleaseSourceError> {
        let output = match Command::new("curl").args(arguments).output() {
            Ok(output) => output,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(ReleaseSourceError::CurlUnavailable);
            }
            Err(error) => {
                return Err(ReleaseSourceError::TransportFailed {
                    detail: format!("could not run curl: {error}"),
                });
            }
        };
        if !output.status.success() {
            return Err(ReleaseSourceError::TransportFailed {
                detail: format!(
                    "curl exited with {}: {}",
                    output.status,
                    String::from_utf8_lossy(&output.stderr).trim()
                ),
            });
        }
        Ok(output.stdout)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A response shaped like the real API's, covering both channels.
    const FEED: &str = r#"[
        {
            "tag_name": "v1.10.0",
            "prerelease": false,
            "draft": false,
            "assets": [
                {"name": "kira-1.10.0-aarch64-macos.tar.gz",
                 "browser_download_url": "https://example.invalid/1.10.0-macos"},
                {"name": "kira-1.10.0-x86_64-linux-gnu.tar.gz",
                 "browser_download_url": "https://example.invalid/1.10.0-linux"}
            ]
        },
        {
            "tag_name": "v1.7.3",
            "prerelease": false,
            "draft": false,
            "assets": [
                {"name": "kira-1.7.3-aarch64-macos.tar.gz",
                 "browser_download_url": "https://example.invalid/1.7.3-macos"}
            ]
        },
        {
            "tag_name": "2026.07.2",
            "prerelease": true,
            "draft": false,
            "assets": [
                {"name": "kira-2026.07.2-aarch64-macos.tar.gz",
                 "browser_download_url": "https://example.invalid/dev"}
            ]
        },
        {
            "tag_name": "v9.9.9",
            "prerelease": false,
            "draft": true,
            "assets": []
        }
    ]"#;

    #[test]
    fn maps_prereleases_to_dev_and_strips_the_tag_prefix() {
        let entries = parse_release_feed(FEED).expect("canned feed parses");
        assert_eq!(entries.len(), 3, "the draft release is skipped");
        assert_eq!(entries[0].version, "1.10.0");
        assert_eq!(entries[0].channel, Channel::Release);
        assert_eq!(entries[2].version, "2026.07.2");
        assert_eq!(entries[2].channel, Channel::Dev);
    }

    #[test]
    fn separates_the_channels_and_orders_each_newest_first() {
        let entries = parse_release_feed(FEED).expect("canned feed parses");
        let release: Vec<String> = releases_on_channel(&entries, Channel::Release)
            .into_iter()
            .map(|entry| entry.version)
            .collect();
        assert_eq!(release, ["1.10.0", "1.7.3"]);
        let dev: Vec<String> = releases_on_channel(&entries, Channel::Dev)
            .into_iter()
            .map(|entry| entry.version)
            .collect();
        assert_eq!(dev, ["2026.07.2"]);
    }

    #[test]
    fn selects_the_asset_matching_the_host_key() {
        let entries = parse_release_feed(FEED).expect("canned feed parses");
        assert_eq!(
            select_asset(&entries[0], "x86_64-linux-gnu").expect("linux asset is published"),
            "https://example.invalid/1.10.0-linux"
        );
        let error = select_asset(&entries[0], "x86_64-windows-msvc")
            .expect_err("no windows asset in this feed");
        assert!(matches!(
            error,
            ReleaseSourceError::MissingHostArtifact { .. }
        ));
    }

    #[test]
    fn rejects_a_feed_that_is_not_the_documented_shape() {
        assert!(matches!(
            parse_release_feed("not json").expect_err("garbage is not a feed"),
            ReleaseSourceError::MalformedFeed { .. }
        ));
        assert!(matches!(
            parse_release_feed(r#"{"releases": []}"#).expect_err("an object is not a feed"),
            ReleaseSourceError::MalformedFeed { .. }
        ));
        assert!(matches!(
            parse_release_feed(r#"[{"prerelease": false}]"#).expect_err("a release needs a tag"),
            ReleaseSourceError::MalformedFeed { .. }
        ));
        assert!(matches!(
            parse_release_feed(r#"[{"tag_name": "v1", "assets": [{"name": "a"}]}]"#)
                .expect_err("an asset needs a download url"),
            ReleaseSourceError::MalformedFeed { .. }
        ));
    }

    #[test]
    fn builds_the_feed_url_from_the_repository() {
        let source = GitHubReleaseSource::new(DEFAULT_REPOSITORY, "aarch64-macos");
        assert_eq!(
            source.feed_url(),
            "https://api.github.com/repos/kira-lang-com/kira/releases?per_page=100"
        );
    }
}
