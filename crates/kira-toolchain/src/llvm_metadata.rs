//! The pinned LLVM toolchain metadata: the parsed form of the repo-root
//! `llvm-metadata.toml`.
//!
//! That file is the source of truth for which LLVM Kira expects, which release
//! owns the published bundles, and the exact archive name per supported host.
//! It is compiled in with `include_str!` rather than read from disk at runtime:
//! an installed `kirac` has no repo to read, and baking it in means the pin can
//! never disagree with the binary that was built from it. Editing the TOML and
//! rebuilding is the whole update flow.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use serde::Deserialize;

/// The raw text of the repo-root `llvm-metadata.toml`, compiled in.
const METADATA_TOML: &str = include_str!("../../../llvm-metadata.toml");

/// The parsed `llvm-metadata.toml`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LlvmMetadata {
    /// Format version of this file's schema.
    pub schema_version: u32,
    /// The pinned LLVM version and the release that owns its bundles.
    pub llvm: LlvmPin,
    /// How the bundles are built, per the release workflow.
    pub build: BuildSettings,
    /// The supported host targets, keyed by bundle key (e.g. `aarch64-macos`).
    pub target: BTreeMap<String, TargetBundle>,
}

/// The pinned LLVM version and its owning release tags.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LlvmPin {
    /// The LLVM version Kira expects (e.g. `22.1.4`).
    pub version: String,
    /// The upstream LLVM source tag the bundles are built from.
    pub source_tag: String,
    /// The Kira-controlled GitHub release tag that owns the published bundles.
    pub release_tag: String,
}

/// The build settings the release workflow uses for every bundle.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct BuildSettings {
    /// CMake build type (e.g. `Release`).
    pub build_type: String,
    /// CMake generator (e.g. `Ninja`).
    pub cmake_generator: String,
    /// Which LLVM targets to build (e.g. `host`).
    pub targets_to_build: String,
}

/// One supported host target's published bundle.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct TargetBundle {
    /// The GitHub-hosted runner that builds this bundle.
    pub runner: String,
    /// The platform family (`windows`, `linux`, `macos`).
    pub platform: String,
    /// The archive format (`zip`, `tar.xz`).
    pub archive: String,
    /// The exact published asset filename. The workflow must match this.
    pub asset: String,
}

/// The pinned metadata, parsed once.
///
/// # Panics
/// Only if the compiled-in `llvm-metadata.toml` is malformed, which is a build
/// -time authoring error rather than a runtime condition: the same bytes are
/// parsed on every run, so a valid file can never fail here in the field. The
/// `parses_the_pinned_metadata` test covers it.
pub fn pinned() -> &'static LlvmMetadata {
    static PINNED: OnceLock<LlvmMetadata> = OnceLock::new();
    PINNED.get_or_init(|| {
        toml::from_str(METADATA_TOML).expect("repo-root llvm-metadata.toml is valid and complete")
    })
}

/// The pinned LLVM version (e.g. `22.1.4`).
pub fn pinned_version() -> &'static str {
    &pinned().llvm.version
}

/// The published bundle for a host bundle key, if that host is supported.
pub fn bundle_for(host_key: &str) -> Option<&'static TargetBundle> {
    pinned().target.get(host_key)
}

/// The `llvm-sys` major that pairs with the pinned LLVM version.
///
/// `llvm-sys` majors track LLVM `major.minor` with the dots removed, so LLVM
/// `22.1.4` pairs with `llvm-sys` `221.x`. Returns `None` if the pinned version
/// is not `major.minor.patch`.
pub fn expected_llvm_sys_major() -> Option<String> {
    let mut parts = pinned_version().split('.');
    let major = parts.next()?;
    let minor = parts.next()?;
    (!major.is_empty() && !minor.is_empty()).then(|| format!("{major}{minor}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_pinned_metadata() {
        let metadata = pinned();
        assert_eq!(metadata.schema_version, 1);
        assert!(!metadata.llvm.version.is_empty());
        assert!(metadata.llvm.source_tag.contains(&metadata.llvm.version));
        assert!(metadata.llvm.release_tag.contains(&metadata.llvm.version));
    }

    #[test]
    fn every_documented_host_has_a_matching_asset_name() {
        // The workflow never invents asset names: each supported host's asset
        // must carry the pinned version and its own bundle key.
        for key in ["x86_64-windows-msvc", "x86_64-linux-gnu", "aarch64-macos"] {
            let bundle = bundle_for(key).expect("supported host is present in the metadata");
            assert!(
                bundle.asset.contains(pinned_version()) && bundle.asset.contains(key),
                "asset `{}` must name the pinned version and host key `{key}`",
                bundle.asset,
            );
        }
        assert!(bundle_for("aarch64-linux-gnu").is_none());
    }

    /// The pinned LLVM and the `llvm-sys` bindings must never drift apart: the
    /// backend links whatever `llvm-sys` was built against, and the managed
    /// bundle is what it will find. This reads the workspace manifest so the
    /// two pins are checked against each other, not against a copy of one.
    #[test]
    fn llvm_sys_dependency_matches_the_pinned_llvm() {
        const WORKSPACE_MANIFEST: &str = include_str!("../../../Cargo.toml");
        let expected_major =
            expected_llvm_sys_major().expect("pinned version is major.minor.patch");
        let line = WORKSPACE_MANIFEST
            .lines()
            .map(str::trim)
            .find(|line| line.starts_with("llvm-sys"))
            .expect("workspace declares an llvm-sys dependency");
        assert!(
            line.contains(&format!("\"{expected_major}.")),
            "llvm-sys pin `{line}` does not match pinned LLVM {} (expected major {expected_major})",
            pinned_version(),
        );
    }
}
