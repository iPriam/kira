//! Provisions the managed libffi archives for `knvm install libffi`.
//!
//! Kira links libffi statically, so the engine is a build-time input rather than
//! something shipped beside an artifact: a user downloads Kira and has it
//! already. The archives live at `<toolchains-root>/libffi/<version>/<target>`,
//! the path `kira-libffi`'s build script links out of.
//!
//! Every published target is installed, not just this host's. The archives are
//! tens of kilobytes each, and installing only the host's would make
//! `kira build --target aarch64-linux-gnu` work or not depending on which
//! machine you were sitting at — the same property the LLVM pin refuses for
//! code generators, for the same reason.

use std::path::{Path, PathBuf};

use kira_toolchain::LibffiArchive;

use crate::digest::{Sha256, checksum_file_name};
use crate::github;
use crate::install::{InstallError, Staging};
use crate::source::{ReleaseSourceError, read_published_checksum};
use crate::unpack::{self, UnpackError};

/// Why a libffi archive could not be provisioned.
#[derive(Debug, thiserror::Error)]
pub enum LibffiInstallError {
    /// The compiled-in pin could not be read.
    #[error(transparent)]
    Metadata(#[from] kira_toolchain::MalformedMetadata),
    /// KIRA_HOME could not be resolved.
    #[error(transparent)]
    Home(#[from] kira_toolchain::HomeDirectoryUnavailable),
    /// The release exists but does not carry a target's archive.
    #[error("release `{tag}` publishes no `{asset}`")]
    MissingAsset {
        /// The release tag named by the pin.
        tag: String,
        /// The asset filename named by the pin.
        asset: String,
    },
    /// The unpacked archive is not a libffi install tree.
    #[error("the unpacked archive for `{target}` holds no `lib/{archive}`; nothing was installed")]
    NotALibffiTree {
        /// The target being installed.
        target: String,
        /// The archive file that was looked for.
        archive: String,
    },
    /// The downloaded archive is not the one the release published.
    #[error(
        "`{asset}` does not match the checksum published for it\n  \
         published: {expected}\n  \
         downloaded: {actual}\n\
         The download is corrupt or the archive was changed after publication; \
         nothing was installed"
    )]
    ChecksumMismatch {
        /// The asset that was fetched.
        asset: String,
        /// What the release published.
        expected: Sha256,
        /// What arrived.
        actual: Sha256,
    },
    /// The archive could not be unpacked.
    #[error(transparent)]
    Unpack(#[from] UnpackError),
    /// The release feed could not be read.
    #[error(transparent)]
    ReleaseSource(#[from] ReleaseSourceError),
    /// A filesystem or network step failed.
    #[error(transparent)]
    Install(#[from] InstallError),
}

/// One target's installed archive, as the verb reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LibffiInstalled {
    /// The target key, as the pin spells it.
    pub target: String,
    /// Where the archive was installed.
    pub home: PathBuf,
    /// Whether it was already there and left alone.
    pub already_installed: bool,
    /// The published checksum, when the release carried one to check against.
    pub verified: Option<Sha256>,
}

/// `<toolchains-root>/libffi/<version>/<target>` — where one target's archive
/// is installed.
///
/// Derived here as well as in `kira-toolchain` so a test can name the path
/// without a KIRA_HOME, and asserted equal to that one.
#[must_use]
pub fn libffi_home(toolchains_root: &Path, version: &str, target: &str) -> PathBuf {
    toolchains_root.join("libffi").join(version).join(target)
}

/// The operating system and ABI a target key stands for.
///
/// Only what the archive's file name turns on: MSVC writes `ffi.lib` and
/// everything else `libffi.a`. Windows archives in the pin are the MSVC-built
/// ones, which is why that is the ABI reported for them.
fn target_os_and_env(target: &str) -> (&str, &str) {
    match target.split('-').next().unwrap_or_default() {
        "windows" => ("windows", "msvc"),
        "macos" => ("macos", ""),
        _ => ("linux", "gnu"),
    }
}

/// Installs the pinned libffi archive for every published target.
///
/// A target already installed is left alone unless `force`, and reported as
/// such: the archives never change for a given version, so re-fetching them is
/// work with no result.
pub fn install_libffi(
    toolchains_root: &Path,
    force: bool,
) -> Result<Vec<LibffiInstalled>, LibffiInstallError> {
    let pin = kira_toolchain::libffi_pinned()?;
    let mut installed = Vec::new();
    for (target, archive) in &pin.target {
        installed.push(install_one(
            toolchains_root,
            &pin.libffi.version,
            &pin.libffi.repository,
            &pin.libffi.release_tag,
            target,
            archive,
            force,
        )?);
    }
    Ok(installed)
}

fn install_one(
    toolchains_root: &Path,
    version: &str,
    repository: &str,
    tag: &str,
    target: &str,
    published: &LibffiArchive,
    force: bool,
) -> Result<LibffiInstalled, LibffiInstallError> {
    let (os, env) = target_os_and_env(target);
    let home = libffi_home(toolchains_root, version, target);

    if kira_toolchain::is_libffi_home(&home, os, env) {
        if !force {
            return Ok(LibffiInstalled {
                target: target.to_string(),
                home,
                already_installed: true,
                verified: None,
            });
        }
        std::fs::remove_dir_all(&home)
            .map_err(|error| InstallError::io("remove the previous archive at", &home, error))?;
    }

    let staging = Staging::create(toolchains_root)?;
    let (downloaded, verified) = fetch_archive(repository, tag, published, staging.path())?;

    let unpacked = staging.path().join("unpacked");
    unpack::extract(&downloaded, &unpacked, &published.archive)?;
    let payload = locate_tree(&unpacked, target, os, env)?;

    if let Some(parent) = home.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| InstallError::io("create", parent, error))?;
    }
    std::fs::rename(&payload, &home)
        .map_err(|error| InstallError::io("move the unpacked archive into", &home, error))?;

    Ok(LibffiInstalled {
        target: target.to_string(),
        home,
        already_installed: false,
        verified,
    })
}

fn fetch_archive(
    repository: &str,
    tag: &str,
    published: &LibffiArchive,
    into: &Path,
) -> Result<(PathBuf, Option<Sha256>), LibffiInstallError> {
    let release = github::parse_release_by_tag(&github::get_text(&github::release_by_tag_url(
        repository, tag,
    ))?)?;
    let url = github::asset_named(&release, &published.asset).ok_or_else(|| {
        LibffiInstallError::MissingAsset {
            tag: tag.to_string(),
            asset: published.asset.clone(),
        }
    })?;

    std::fs::create_dir_all(into).map_err(|error| InstallError::io("create", into, error))?;
    let archive = into.join(&published.asset);
    github::download(url, &archive)?;

    // Checked before the unpacker sees the bytes, for the reason the LLVM
    // bundle is: this file gets linked into every native artifact built
    // afterwards, so an archive that is not the published one must not reach a
    // build at all.
    let sidecar = checksum_file_name(&published.asset);
    let verified = match github::asset_named(&release, &sidecar) {
        None => None,
        Some(sidecar_url) => {
            let expected =
                read_published_checksum(&published.asset, &github::get_text(sidecar_url)?)?;
            let actual = Sha256::of_file(&archive).map_err(|error| {
                InstallError::io("read the downloaded archive", &archive, error)
            })?;
            if actual != expected {
                return Err(LibffiInstallError::ChecksumMismatch {
                    asset: published.asset.clone(),
                    expected,
                    actual,
                });
            }
            Some(expected)
        }
    };

    Ok((archive, verified))
}

/// Finds the install tree inside an unpacked archive.
///
/// The published archives wrap the tree in one directory named for the asset;
/// a tree at the top level is accepted too. The test either way is the one the
/// build script applies, so nothing can be installed that a build would then
/// not find.
fn locate_tree(
    unpacked: &Path,
    target: &str,
    os: &str,
    env: &str,
) -> Result<PathBuf, LibffiInstallError> {
    if kira_toolchain::is_libffi_home(unpacked, os, env) {
        return Ok(unpacked.to_path_buf());
    }
    let entries =
        std::fs::read_dir(unpacked).map_err(|error| InstallError::io("read", unpacked, error))?;
    for entry in entries.flatten() {
        let path = entry.path();
        if kira_toolchain::is_libffi_home(&path, os, env) {
            return Ok(path);
        }
    }
    Err(LibffiInstallError::NotALibffiTree {
        target: target.to_string(),
        archive: kira_toolchain::static_archive_name_for(os, env).to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derives_the_home_the_build_script_links_out_of() {
        let root = Path::new("/toolchains");
        assert_eq!(
            libffi_home(root, "3.5.2", "linux-aarch64"),
            Path::new("/toolchains/libffi/3.5.2/linux-aarch64")
        );
    }

    /// The archive's name turns on the ABI, and getting it wrong installs a
    /// tree the build script then reports as missing.
    #[test]
    fn windows_archives_are_the_msvc_spelling_and_the_rest_are_not() {
        assert_eq!(target_os_and_env("windows-x86_64"), ("windows", "msvc"));
        assert_eq!(target_os_and_env("windows-aarch64"), ("windows", "msvc"));
        assert_eq!(target_os_and_env("linux-aarch64"), ("linux", "gnu"));
        assert_eq!(target_os_and_env("macos-aarch64"), ("macos", ""));
    }

    /// Every target in the pin has to resolve to an ABI whose archive name the
    /// published asset actually carries.
    #[test]
    fn every_pinned_target_names_an_archive_the_build_script_would_look_for() {
        for target in kira_toolchain::libffi_pinned().unwrap().target.keys() {
            let (os, env) = target_os_and_env(target);
            let expected = kira_toolchain::static_archive_name_for(os, env);
            assert!(
                expected == "libffi.a" || expected == "libffi.lib",
                "`{target}` resolved to an unexpected archive name `{expected}`"
            );
        }
    }
}
