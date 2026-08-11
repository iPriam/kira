//! The pinned LLVM toolchain metadata: the parsed form of the repo-root
//! `llvm-metadata.toml`.
//!
//! That file is the source of truth for which LLVM Kira expects, which release
//! owns the published bundles, and the exact archive name per supported host.
//! It is compiled in with `include_str!` rather than read from disk at runtime:
//! an installed `kira` has no repo to read, and baking it in means the pin can
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

/// The compiled-in `llvm-metadata.toml` could not be parsed.
///
/// An authoring error in this repo, not a condition of the machine running
/// `kira`: the same bytes are parsed on every run, so this is either always
/// hit or never. It is still a typed error rather than a panic, because a
/// library does not get to decide that its caller's process should end.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("the compiled-in `llvm-metadata.toml` is malformed: {detail}")]
pub struct MalformedMetadata {
    /// What the TOML parser objected to.
    pub detail: String,
}

/// The pinned metadata, parsed once.
pub fn pinned() -> Result<&'static LlvmMetadata, MalformedMetadata> {
    static PINNED: OnceLock<Result<LlvmMetadata, MalformedMetadata>> = OnceLock::new();
    PINNED
        .get_or_init(|| {
            toml::from_str(METADATA_TOML).map_err(|error| MalformedMetadata {
                detail: error.to_string(),
            })
        })
        .as_ref()
        .map_err(Clone::clone)
}

/// The pinned LLVM version (e.g. `22.1.4`).
pub fn pinned_version() -> Result<&'static str, MalformedMetadata> {
    Ok(&pinned()?.llvm.version)
}

/// The published bundle for a host bundle key, if that host is supported.
pub fn bundle_for(host_key: &str) -> Result<Option<&'static TargetBundle>, MalformedMetadata> {
    Ok(pinned()?.target.get(host_key))
}

/// The `llvm-sys` major that pairs with the pinned LLVM version.
///
/// `llvm-sys` majors track LLVM `major.minor` with the dots removed, so LLVM
/// `22.1.4` pairs with `llvm-sys` `221.x`. Returns `Ok(None)` if the pinned
/// version is not `major.minor.patch`.
pub fn expected_llvm_sys_major() -> Result<Option<String>, MalformedMetadata> {
    let version = pinned_version()?;
    let mut parts = version.split('.');
    let (Some(major), Some(minor)) = (parts.next(), parts.next()) else {
        return Ok(None);
    };
    Ok((!major.is_empty() && !minor.is_empty()).then(|| format!("{major}{minor}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The compiled-in file is parsed on every run, so this failing means the
    /// repo's own `llvm-metadata.toml` is broken — which is exactly the
    /// authoring error the typed result exists to report rather than panic on.
    #[test]
    fn parses_the_pinned_metadata() {
        let metadata = pinned().expect("the repo's llvm-metadata.toml parses");
        assert_eq!(metadata.schema_version, 1);
        assert!(!metadata.llvm.version.is_empty());
        assert!(metadata.llvm.source_tag.contains(&metadata.llvm.version));
        assert!(metadata.llvm.release_tag.contains(&metadata.llvm.version));
    }

    #[test]
    fn every_documented_host_has_a_matching_asset_name() {
        // The workflow never invents asset names: each supported host's asset
        // must carry the pinned version and its own bundle key.
        let version = pinned_version().expect("the pin parses");
        for key in ["x86_64-windows-msvc", "x86_64-linux-gnu", "aarch64-macos"] {
            let bundle = bundle_for(key)
                .expect("the pin parses")
                .expect("supported host is present in the metadata");
            assert!(
                bundle.asset.contains(version) && bundle.asset.contains(key),
                "asset `{}` must name the pinned version and host key `{key}`",
                bundle.asset,
            );
        }
        assert!(
            bundle_for("aarch64-linux-gnu")
                .expect("the pin parses")
                .is_none()
        );
    }

    /// MSVC compatibility runs forward only, so a Windows bundle links on
    /// nothing older than the toolset that built it unless the build refuses
    /// the STL's toolset-private helpers and the release proves none survived.
    /// That pair is what lets the runner image float; drop either half and the
    /// floor silently becomes whatever GitHub last installed, which is how a
    /// published bundle came to name STL symbols no released Visual Studio
    /// defined.
    #[test]
    fn the_windows_bundle_earns_its_floating_runner() {
        const BUILD_SCRIPT: &str = include_str!("../../../scripts/llvm/build-llvm.ps1");
        const RELEASE_WORKFLOW: &str =
            include_str!("../../../.github/workflows/release-llvm-toolchains.yml");
        assert!(
            BUILD_SCRIPT.contains("_USE_STD_VECTOR_ALGORITHMS=0"),
            "build-llvm.ps1 must opt out of the vectorized STL algorithms, \
             whose helpers live in the building toolset's own library"
        );
        assert!(
            RELEASE_WORKFLOW.contains("check-msvc-portability.ps1"),
            "the release workflow must check the built bundle against its \
             compatibility floor before publishing it"
        );
    }

    /// The pinned LLVM and the `llvm-sys` bindings must never drift apart: the
    /// backend links whatever `llvm-sys` was built against, and the managed
    /// bundle is what it will find. This reads the workspace manifest so the
    /// two pins are checked against each other, not against a copy of one.
    #[test]
    fn llvm_sys_dependency_matches_the_pinned_llvm() {
        const WORKSPACE_MANIFEST: &str = include_str!("../../../Cargo.toml");
        let version = pinned_version().expect("the pin parses");
        let expected_major = expected_llvm_sys_major()
            .expect("the pin parses")
            .expect("pinned version is major.minor.patch");
        let line = WORKSPACE_MANIFEST
            .lines()
            .map(str::trim)
            .find(|line| line.starts_with("llvm-sys"))
            .expect("workspace declares an llvm-sys dependency");
        assert!(
            line.contains(&format!("\"{expected_major}.")),
            "llvm-sys pin `{line}` does not match pinned LLVM {} (expected major {expected_major})",
            version,
        );
    }
}
