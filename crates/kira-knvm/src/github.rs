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

use crate::digest::{Sha256, checksum_file_name};
use crate::source::{
    ReleaseSource, ReleaseSourceError, archive_file_name, compare_versions, read_published_checksum,
};

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
        // A repository publishes more than its own toolchain: the managed LLVM
        // bundles are releases here too, tagged `llvm-v<version>-kira.<n>`.
        // They are not versions of Kira, and reading them as versions made
        // `install latest` resolve one and then ask for a Kira archive it does
        // not carry. The by-tag reader below does not filter, because that is
        // how those bundles are fetched — by the tag their provisioner knows.
        //
        // Only a tag that is present and names something else is skipped. A
        // release the feed gives no readable tag at all is the shape changing
        // underneath us, and that is `parse_release`'s error to raise rather
        // than one release quietly fewer.
        if let Some(tag) = release.get("tag_name").and_then(serde_json::Value::as_str)
            && !names_a_kira_release(tag)
        {
            continue;
        }
        if let Some(entry) = parse_release(release)? {
            entries.push(entry);
        }
    }
    Ok(entries)
}

/// Whether a tag names a release of Kira itself.
///
/// A Kira tag is a dotted number, with the optional leading `v` that
/// [`strip_tag_prefix`] already takes both ways, and an optional prerelease
/// suffix: `v1.8.3`, `1.8.3`, `v1.8.0.1`, `v1.8.0-dev5`. Anything the leading
/// component is not a number of belongs to something else the repository
/// publishes.
#[must_use]
pub fn names_a_kira_release(tag: &str) -> bool {
    let number = strip_tag_prefix(tag).split('-').next().unwrap_or_default();
    !number.is_empty()
        && number
            .split('.')
            .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
}

/// Parses a GitHub `/releases/tags/<tag>` API response into one release.
///
/// The by-tag endpoint answers with a single object rather than an array. It
/// is what the LLVM provisioner asks, because it already knows the tag its
/// bundles are published under and has no use for the whole feed.
pub fn parse_release_by_tag(json: &str) -> Result<ReleaseEntry, ReleaseSourceError> {
    let value: serde_json::Value =
        serde_json::from_str(json).map_err(|error| ReleaseSourceError::MalformedFeed {
            detail: format!("response is not JSON: {error}"),
        })?;
    parse_release(&value)?.ok_or_else(|| ReleaseSourceError::MalformedFeed {
        detail: "the release named by that tag is a draft".to_string(),
    })
}

/// One release object, or `None` when it is a draft.
///
/// Shared by the feed and by-tag readers so both answer the same way about
/// what a release is — a second copy would be a second contract.
fn parse_release(release: &serde_json::Value) -> Result<Option<ReleaseEntry>, ReleaseSourceError> {
    if release.get("draft").and_then(serde_json::Value::as_bool) == Some(true) {
        return Ok(None);
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

    Ok(Some(ReleaseEntry {
        version: strip_tag_prefix(tag).to_string(),
        channel: if prerelease {
            Channel::Dev
        } else {
            Channel::Release
        },
        assets,
    }))
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
    asset_named(release, &wanted).ok_or(ReleaseSourceError::MissingHostArtifact {
        version: release.version.clone(),
        artifact: wanted,
    })
}

/// The download URL of the checksum sidecar for this host's asset.
///
/// `None` when the release publishes no sidecar — releases cut before sidecars
/// existed have none, and those install as unverified rather than failing.
#[must_use]
pub fn select_checksum_asset<'a>(release: &'a ReleaseEntry, host_key: &str) -> Option<&'a str> {
    asset_named(
        release,
        &checksum_file_name(&archive_file_name(&release.version, host_key)),
    )
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

    /// The host key this source installs artifacts for.
    #[must_use]
    pub fn host_key(&self) -> &str {
        &self.host_key
    }

    /// The whole feed, parsed.
    pub(crate) fn entries(&self) -> Result<Vec<ReleaseEntry>, ReleaseSourceError> {
        parse_release_feed(&transport::get_text(&self.feed_url())?)
    }

    /// One channel's release by version, or a typed refusal naming both.
    pub(crate) fn release_on(
        &self,
        channel: Channel,
        version: &str,
    ) -> Result<ReleaseEntry, ReleaseSourceError> {
        releases_on_channel(&self.entries()?, channel)
            .into_iter()
            .find(|entry| entry.version == version)
            .ok_or_else(|| ReleaseSourceError::VersionNotFound {
                channel: channel.dir_name(),
                version: version.to_string(),
            })
    }
}

/// The API URL of one release, named by its tag.
///
/// The LLVM provisioner knows the tag its bundles are published under, so it
/// asks for that release rather than reading and filtering the whole feed.
#[must_use]
pub fn release_by_tag_url(repository: &str, tag: &str) -> String {
    format!("https://api.github.com/repos/{repository}/releases/tags/{tag}")
}

/// The download URL of a release asset with exactly this name.
#[must_use]
pub fn asset_named<'a>(release: &'a ReleaseEntry, name: &str) -> Option<&'a str> {
    release
        .assets
        .iter()
        .find(|asset| asset.name == name)
        .map(|asset| asset.url.as_str())
}

/// Fetches a URL as text.
///
/// The one transport, shared: the LLVM provisioner downloads from the same
/// host as a toolchain install and has no reason to grow a second copy.
pub(crate) fn get_text(url: &str) -> Result<String, ReleaseSourceError> {
    transport::get_text(url)
}

/// Downloads a URL to a file.
pub(crate) fn download(url: &str, destination: &Path) -> Result<(), ReleaseSourceError> {
    transport::download(url, destination)
}

impl ReleaseSource for GitHubReleaseSource {
    fn available_versions(&self, channel: Channel) -> Result<Vec<String>, ReleaseSourceError> {
        let versions: Vec<String> = releases_on_channel(&self.entries()?, channel)
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
        let release = self.release_on(channel, version)?;
        let url = select_asset(&release, &self.host_key)?;

        std::fs::create_dir_all(dest_dir)
            .map_err(|error| ReleaseSourceError::io("create", dest_dir, error))?;
        let destination = dest_dir.join(archive_file_name(version, &self.host_key));
        transport::download(url, &destination)?;
        Ok(destination)
    }

    fn published_checksum(
        &self,
        channel: Channel,
        version: &str,
    ) -> Result<Option<Sha256>, ReleaseSourceError> {
        let release = self.release_on(channel, version)?;
        let Some(url) = select_checksum_asset(&release, &self.host_key) else {
            return Ok(None);
        };
        let contents = transport::get_text(url)?;
        read_published_checksum(&archive_file_name(version, &self.host_key), &contents).map(Some)
    }
}

/// The variables a GitHub token is read from, in the order `gh` resolves them.
///
/// A machine already configured for `gh` is configured for knvm, and a CI job
/// that exports the workflow token gets the authenticated rate limit without
/// naming a knvm-specific variable.
const TOKEN_VARIABLES: [&str; 2] = ["GH_TOKEN", "GITHUB_TOKEN"];

/// The token to send with a request, or `None` to send none.
///
/// Unauthenticated GitHub allows sixty API requests an hour per address, which
/// is shared: on a CI runner, behind one office address, or behind a carrier
/// NAT, an install fails on somebody else's traffic. A token raises that to
/// five thousand, and every such environment already has one.
fn token_for(url: &str) -> Option<String> {
    if !is_github_host(url) {
        return None;
    }
    TOKEN_VARIABLES.iter().find_map(|name| {
        std::env::var(name)
            .ok()
            .filter(|value| !value.trim().is_empty())
    })
}

/// Whether a URL addresses GitHub itself.
///
/// A token is a credential, and an asset's bytes come from a storage host that
/// never asked for one. `curl` drops the header across a redirect to another
/// host, and this refuses to set it for a URL that was never GitHub's to begin
/// with — a release source pointed elsewhere must not be handed the token.
fn is_github_host(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("https://") else {
        return false;
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    // A URL may carry `user@host`, and the host is what a credential is scoped
    // to: `https://api.github.com@example.com/` addresses `example.com`.
    let host = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host)
        .split(':')
        .next()
        .unwrap_or_default();
    host == "github.com" || host.ends_with(".github.com")
}

/// The only code in this crate that talks to a network.
///
/// Isolated so that replacing `curl` with a native client is a change to this
/// module alone, and so that everything above it is reachable by tests.
mod transport {
    use super::{Command, Path, ReleaseSourceError, TRANSFER_TIMEOUT_SECONDS, token_for};

    /// Fetches a URL as text.
    pub(super) fn get_text(url: &str) -> Result<String, ReleaseSourceError> {
        let mut arguments = vec![
            "-fsSL",
            "--max-time",
            TRANSFER_TIMEOUT_SECONDS,
            "-H",
            "User-Agent: knvm",
            "-H",
            "Accept: application/vnd.github+json",
        ];
        let authorization = authorization_header(url);
        if let Some(header) = authorization.as_deref() {
            arguments.extend(["-H", header]);
        }
        arguments.push(url);
        let output = run(&arguments)?;
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
        let mut arguments = vec![
            "-fsSL",
            "--max-time",
            TRANSFER_TIMEOUT_SECONDS,
            "-H",
            "User-Agent: knvm",
            "-o",
            destination_text,
        ];
        let authorization = authorization_header(url);
        if let Some(header) = authorization.as_deref() {
            arguments.extend(["-H", header]);
        }
        arguments.push(url);
        run(&arguments).map(|_| ())
    }

    /// The `Authorization` header this request carries, when it carries one.
    fn authorization_header(url: &str) -> Option<String> {
        token_for(url).map(|token| format!("Authorization: Bearer {token}"))
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

    /// The token is a credential, so the set of URLs it may be sent to is the
    /// part of this worth testing: a host that merely reads like GitHub's is
    /// not GitHub's.
    #[test]
    fn a_token_addresses_github_and_nothing_that_resembles_it() {
        for url in [
            "https://api.github.com/repos/kira-lang-com/kira/releases",
            "https://github.com/kira-lang-com/kira/releases/download/v1.8.3/kira.tar.gz",
            "https://uploads.github.com/anything",
        ] {
            assert!(is_github_host(url), "{url} is GitHub");
        }
        for url in [
            "https://objects.githubusercontent.com/bytes",
            "https://github.com.example.com/repos",
            "https://api.github.com@example.com/repos",
            "https://example.com/github.com/repos",
            "http://api.github.com/repos",
            "api.github.com/repos",
        ] {
            assert!(!is_github_host(url), "{url} is not GitHub");
        }
    }

    /// The repository publishes the managed LLVM bundles as releases of its
    /// own, and reading one as a version of Kira is what made `install latest`
    /// ask for a Kira archive under an LLVM tag.
    #[test]
    fn the_feed_carries_only_releases_of_kira() {
        let feed = r#"[
            {
                "tag_name": "llvm-v23.1.0-rc3-kira.1",
                "prerelease": false,
                "assets": []
            },
            {
                "tag_name": "v1.8.3",
                "prerelease": false,
                "assets": []
            }
        ]"#;
        let entries = parse_release_feed(feed).expect("the feed parses");
        assert_eq!(
            entries
                .iter()
                .map(|entry| &entry.version)
                .collect::<Vec<_>>(),
            vec!["1.8.3"]
        );
    }

    #[test]
    fn a_kira_tag_is_a_dotted_number_with_or_without_the_v() {
        for tag in ["v1.8.3", "1.8.3", "v1.8.0.1", "v1.8.0-dev5", "2026.07.2"] {
            assert!(names_a_kira_release(tag), "{tag} is Kira's");
        }
        for tag in [
            "llvm-v23.1.0-rc3-kira.1",
            "libffi-v3.5.2",
            "v",
            "v-dev1",
            "vnext",
            "v1..2",
        ] {
            assert!(!names_a_kira_release(tag), "{tag} is not Kira's");
        }
    }

    /// A port does not change which host a URL addresses.
    #[test]
    fn a_port_leaves_the_host_intact() {
        assert!(is_github_host("https://api.github.com:443/repos"));
        assert!(!is_github_host("https://example.com:443/api.github.com"));
    }
}
