//! The pinned libffi engine metadata: the parsed form of the repo-root
//! `libffi-metadata.toml`.
//!
//! Compiled in with `include_str!` for the same reason the LLVM pin is: an
//! installed `kira` has no repo to read, and baking it in means the pin can
//! never disagree with the binary built from it.
//!
//! Unlike the LLVM pin, this one is consulted per *target* rather than per host.
//! libffi is linked statically into every native artifact, so the archive a
//! build needs is the one for the machine it is emitting for, which on a cross
//! build is not the machine doing the emitting.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use serde::Deserialize;

use crate::llvm_metadata::MalformedMetadata;

/// The raw text of the repo-root `libffi-metadata.toml`, compiled in.
const METADATA_TOML: &str = include_str!("../../../libffi-metadata.toml");

/// The parsed `libffi-metadata.toml`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LibffiMetadata {
    /// Format version of this file's schema.
    pub schema_version: u32,
    /// The pinned libffi version and the release that owns its archives.
    pub libffi: LibffiPin,
    /// The supported targets, keyed as `libffi_vendor_target` spells them.
    pub target: BTreeMap<String, LibffiArchive>,
}

/// The pinned libffi version and its owning release.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LibffiPin {
    /// The libffi version Kira links (e.g. `3.5.2`).
    pub version: String,
    /// The GitHub repository whose release owns the published archives.
    pub repository: String,
    /// The release tag that owns them.
    pub release_tag: String,
}

/// One target's published static archive.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LibffiArchive {
    /// The platform family (`windows`, `linux`, `macos`).
    pub platform: String,
    /// The archive format (`tar.gz`, `zip`).
    pub archive: String,
    /// The exact published asset filename. The workflow must match this.
    pub asset: String,
}

/// The pinned metadata, parsed once.
pub fn pinned() -> Result<&'static LibffiMetadata, MalformedMetadata> {
    static PINNED: OnceLock<Result<LibffiMetadata, MalformedMetadata>> = OnceLock::new();
    PINNED
        .get_or_init(|| {
            toml::from_str(METADATA_TOML).map_err(|error| MalformedMetadata {
                detail: error.to_string(),
            })
        })
        .as_ref()
        .map_err(Clone::clone)
}

/// The pinned libffi version (e.g. `3.5.2`).
pub fn pinned_version() -> Result<&'static str, MalformedMetadata> {
    Ok(&pinned()?.libffi.version)
}

/// The published archive for a target key, if that target has one.
pub fn archive_for(target_key: &str) -> Result<Option<&'static LibffiArchive>, MalformedMetadata> {
    Ok(pinned()?.target.get(target_key))
}

/// The file name of the static archive inside an installed libffi home.
///
/// Taken as the target's ABI rather than read from `cfg!`, because a build
/// script links this file for the machine it is compiling for and not for the
/// host it runs on.
///
/// Both spellings keep the `lib` prefix, which on MSVC is not what a reader
/// expects: libffi's own install step writes `libffi.lib` rather than the
/// `ffi.lib` an MSVC library is usually called, and the published archives
/// carry that name. Guessing the conventional spelling here installs a tree the
/// build script then reports as empty.
#[must_use]
pub fn static_archive_name_for(os: &str, env: &str) -> &'static str {
    if os == "windows" && env == "msvc" {
        "libffi.lib"
    } else {
        "libffi.a"
    }
}

/// The name a linker is given for the static archive, without prefix or suffix.
///
/// Follows the file name rather than the platform convention: rustc turns
/// `static=<name>` into `lib<name>.a` for a GNU-style linker and `<name>.lib`
/// for MSVC, so the archive libffi installs as `libffi.lib` is asked for as
/// `libffi` there and as `ffi` everywhere else.
#[must_use]
pub fn link_name_for(os: &str, env: &str) -> &'static str {
    if os == "windows" && env == "msvc" {
        "libffi"
    } else {
        "ffi"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The pin is data this crate compiles in, so a syntax error in it is a
    /// build-time fact rather than a condition of any machine.
    #[test]
    fn the_compiled_in_pin_parses() {
        assert!(pinned().is_ok(), "libffi-metadata.toml must parse");
    }

    /// Every target Kira vendors an engine for has to have an archive to link,
    /// or a build for it fails after the compile rather than before it.
    #[test]
    fn every_supported_target_has_an_archive() {
        for (os, arch) in [
            ("linux", "x86_64"),
            ("linux", "aarch64"),
            ("macos", "aarch64"),
            ("windows", "x86_64"),
        ] {
            let key = crate::libffi_vendor_target(os, arch)
                .unwrap_or_else(|| panic!("{os}/{arch} must have a vendor target"));
            assert!(
                archive_for(key).unwrap().is_some(),
                "`{key}` must name a published archive"
            );
        }
    }

    /// The asset names carry the pinned version, so a version bump that misses
    /// one would fetch an archive of the wrong libffi.
    #[test]
    fn asset_names_carry_the_pinned_version() {
        let metadata = pinned().unwrap();
        for (key, archive) in &metadata.target {
            assert!(
                archive.asset.contains(&metadata.libffi.version),
                "`{key}`'s asset `{}` must name version {}",
                archive.asset,
                metadata.libffi.version
            );
        }
    }

    /// The published MSVC archive is `libffi.lib`, not the `ffi.lib` an MSVC
    /// library is conventionally called. This is the assertion that keeps the
    /// convention from being reintroduced.
    #[test]
    fn msvc_keeps_the_lib_prefix_that_libffi_installs_with() {
        assert_eq!(static_archive_name_for("windows", "msvc"), "libffi.lib");
        assert_eq!(static_archive_name_for("windows", "gnu"), "libffi.a");
        assert_eq!(static_archive_name_for("linux", "gnu"), "libffi.a");
        assert_eq!(static_archive_name_for("macos", ""), "libffi.a");
    }

    /// The name handed to the linker has to reduce back to the file that is
    /// actually on disk, under each linker's own prefixing rules.
    #[test]
    fn the_link_name_reconstructs_the_archive_file_name() {
        for (os, env, expected) in [
            ("windows", "msvc", "libffi.lib"),
            ("linux", "gnu", "libffi.a"),
            ("macos", "", "libffi.a"),
        ] {
            let name = link_name_for(os, env);
            let reconstructed = if os == "windows" && env == "msvc" {
                format!("{name}.lib")
            } else {
                format!("lib{name}.a")
            };
            assert_eq!(reconstructed, expected, "for {os}/{env}");
        }
    }
}
