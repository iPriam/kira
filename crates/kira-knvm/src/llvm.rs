//! Provisioning the managed LLVM bundle: `knvm install-llvm`.
//!
//! The LLVM backend is a hard dependency of every `kira`, and its build script
//! discovers the bundle at `<toolchains-root>/llvm/<version>/<host-key>`
//! without being told where it is. Until now the only things that put a bundle
//! there were a CI step and a developer running `tar` by hand; this is the
//! supported route, and it lands the tree at exactly the path
//! `kira_toolchain::discover` looks in.
//!
//! # What decides the version
//!
//! Nothing here. The pin is `llvm-metadata.toml`, compiled into
//! `kira-toolchain`, which names the LLVM version, the GitHub release tag that
//! owns the published bundles, and the exact asset filename per host. This
//! module reads that and downloads what it says. A knvm built from a checkout
//! whose pin has moved provisions the new bundle by construction — there is no
//! second place recording which LLVM Kira wants.
//!
//! # Why it is not part of a toolchain install
//!
//! `llvm/` is a version-independent sibling of the channel directories, shared
//! by every installed toolchain and keyed by its own version. A toolchain
//! install writes `<channel>/<version>/` and `current.toml` and nothing else,
//! and that separation is asserted by its tests. Provisioning LLVM is
//! therefore its own verb, and it writes only under `llvm/`.

use std::path::{Path, PathBuf};

use kira_toolchain::{TargetBundle, is_llvm_home};

use crate::digest::{Sha256, checksum_file_name};
use crate::github;
use crate::install::{InstallError, Staging};
use crate::source::{ReleaseSourceError, read_published_checksum};

/// Why an LLVM bundle could not be provisioned.
#[derive(Debug, thiserror::Error)]
pub enum LlvmInstallError {
    /// The compiled-in pin could not be read.
    #[error(transparent)]
    Metadata(#[from] kira_toolchain::MalformedMetadata),
    /// This host has no published bundle.
    #[error(
        "no managed LLVM bundle is published for this host ({os}/{arch}); \
         build one with `scripts/llvm/build-llvm.sh` and point KIRA_LLVM_HOME at it"
    )]
    UnsupportedHost {
        /// `std::env::consts::OS`.
        os: &'static str,
        /// `std::env::consts::ARCH`.
        arch: &'static str,
    },
    /// The release exists but does not carry this host's bundle.
    #[error("release `{tag}` publishes no `{asset}`")]
    MissingAsset {
        /// The release tag named by the pin.
        tag: String,
        /// The asset filename named by the pin.
        asset: String,
    },
    /// The archive format named by the pin is not one this can unpack.
    #[error("`{format}` is not an archive format knvm unpacks (expected `tar.xz` or `zip`)")]
    UnsupportedArchiveFormat {
        /// What the pin said.
        format: String,
    },
    /// The unpacking tool is not on this host.
    #[error("`{tool}` was not found on PATH; knvm unpacks `{format}` bundles with it")]
    UnpackerUnavailable {
        /// The tool that was looked for.
        tool: &'static str,
        /// The format it would have unpacked.
        format: String,
    },
    /// The unpacking tool ran and refused the archive.
    #[error("could not unpack `{}`: {detail}", .archive.display())]
    ExtractFailed {
        /// The archive that was being unpacked.
        archive: PathBuf,
        /// What the tool reported.
        detail: String,
    },
    /// The unpacked bundle is not an LLVM install tree.
    #[error(
        "the unpacked bundle is not an LLVM install (no `include/llvm-c/Core.h` \
         under `{}`); nothing was installed",
        .unpacked.display()
    )]
    NotAnLlvmTree {
        /// Where the tree was unpacked.
        unpacked: PathBuf,
    },
    /// The downloaded bundle is not the one the release published.
    #[error(
        "`{asset}` does not match the checksum published for it\n  \
         published: {expected}\n  \
         downloaded: {actual}\n\
         The download is corrupt or the bundle was changed after publication; \
         nothing was installed"
    )]
    ChecksumMismatch {
        /// The asset that was fetched.
        asset: String,
        /// The digest the release publishes.
        expected: Sha256,
        /// The digest of the bytes that arrived.
        actual: Sha256,
    },
    /// The transport failed.
    #[error(transparent)]
    Transport(#[from] ReleaseSourceError),
    /// A filesystem operation failed.
    #[error(transparent)]
    Io(#[from] InstallError),
}

/// What provisioning produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LlvmInstalled {
    /// The LLVM version, as pinned.
    pub version: String,
    /// The host key the bundle was published for.
    pub host_key: &'static str,
    /// `<toolchains-root>/llvm/<version>/<host-key>` — what discovery finds.
    pub home: PathBuf,
    /// Whether a usable bundle was already there, so nothing was fetched.
    pub already_installed: bool,
    /// The digest that was verified, when the release published one.
    pub verified: Option<Sha256>,
}

/// `<toolchains-root>/llvm/<version>/<host-key>`.
///
/// The user-level equivalent in `kira-toolchain` derives its root from
/// `KIRA_HOME`; this takes the root explicitly, so a test provisions into a
/// throwaway directory without touching the process environment.
#[must_use]
pub fn llvm_home(toolchains_root: &Path, version: &str, host_key: &str) -> PathBuf {
    toolchains_root.join("llvm").join(version).join(host_key)
}

/// Downloads and installs the pinned LLVM bundle for this host.
///
/// A usable bundle already at the destination is left alone unless `force`,
/// which removes it and fetches again — the repair route for a tree that was
/// interrupted mid-extraction by something no `Drop` guard runs after.
pub fn install_llvm(
    toolchains_root: &Path,
    repository: &str,
    force: bool,
) -> Result<LlvmInstalled, LlvmInstallError> {
    let pin = kira_toolchain::pinned()?;
    let Some(host_key) = kira_toolchain::llvm_layout::current_host_llvm_bundle_key() else {
        return Err(LlvmInstallError::UnsupportedHost {
            os: std::env::consts::OS,
            arch: std::env::consts::ARCH,
        });
    };
    let Some(bundle) = kira_toolchain::bundle_for(host_key)? else {
        return Err(LlvmInstallError::UnsupportedHost {
            os: std::env::consts::OS,
            arch: std::env::consts::ARCH,
        });
    };

    let home = llvm_home(toolchains_root, &pin.llvm.version, host_key);
    if is_llvm_home(&home) {
        if !force {
            return Ok(LlvmInstalled {
                version: pin.llvm.version.clone(),
                host_key,
                home,
                already_installed: true,
                verified: None,
            });
        }
        std::fs::remove_dir_all(&home)
            .map_err(|error| InstallError::io("remove the previous bundle at", &home, error))?;
    }

    let staging = Staging::create(toolchains_root)?;
    let (archive, verified) =
        fetch_bundle(repository, &pin.llvm.release_tag, bundle, staging.path())?;

    let unpacked = staging.path().join("unpacked");
    extract(&archive, &unpacked, &bundle.archive)?;
    let payload = locate_bundle(&unpacked)?;

    if let Some(parent) = home.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| InstallError::io("create", parent, error))?;
    }
    std::fs::rename(&payload, &home)
        .map_err(|error| InstallError::io("move the unpacked bundle into", &home, error))?;

    Ok(LlvmInstalled {
        version: pin.llvm.version.clone(),
        host_key,
        home,
        already_installed: false,
        verified,
    })
}

/// Downloads the bundle and its sidecar, returning the archive and what was
/// verified.
fn fetch_bundle(
    repository: &str,
    tag: &str,
    bundle: &TargetBundle,
    into: &Path,
) -> Result<(PathBuf, Option<Sha256>), LlvmInstallError> {
    let release = github::parse_release_by_tag(&github::get_text(&github::release_by_tag_url(
        repository, tag,
    ))?)?;
    let url = github::asset_named(&release, &bundle.asset).ok_or_else(|| {
        LlvmInstallError::MissingAsset {
            tag: tag.to_string(),
            asset: bundle.asset.clone(),
        }
    })?;

    std::fs::create_dir_all(into).map_err(|error| InstallError::io("create", into, error))?;
    let archive = into.join(&bundle.asset);
    github::download(url, &archive)?;

    // Verified before the unpacker is handed the bytes, for the same reason a
    // toolchain archive is: a bundle that is not the published one must not be
    // unpacked, let alone installed where every later build will link it.
    let sidecar = checksum_file_name(&bundle.asset);
    let verified = match github::asset_named(&release, &sidecar) {
        None => None,
        Some(sidecar_url) => {
            let expected = read_published_checksum(&bundle.asset, &github::get_text(sidecar_url)?)?;
            let actual = Sha256::of_file(&archive)
                .map_err(|error| InstallError::io("read the downloaded bundle", &archive, error))?;
            if actual != expected {
                return Err(LlvmInstallError::ChecksumMismatch {
                    asset: bundle.asset.clone(),
                    expected,
                    actual,
                });
            }
            Some(expected)
        }
    };

    Ok((archive, verified))
}

/// Unpacks a bundle archive into `destination`.
///
/// `tar` reads `.tar.xz` without being told the compression; `.zip` needs
/// `unzip`, which is what the Windows bundle is packaged as.
fn extract(archive: &Path, destination: &Path, format: &str) -> Result<(), LlvmInstallError> {
    std::fs::create_dir_all(destination)
        .map_err(|error| InstallError::io("create", destination, error))?;

    let (tool, arguments): (&'static str, Vec<&std::ffi::OsStr>) = match format {
        "tar.xz" | "tar.gz" => (
            "tar",
            vec![
                "-xf".as_ref(),
                archive.as_os_str(),
                "-C".as_ref(),
                destination.as_os_str(),
            ],
        ),
        "zip" => (
            "unzip",
            vec![
                "-q".as_ref(),
                archive.as_os_str(),
                "-d".as_ref(),
                destination.as_os_str(),
            ],
        ),
        other => {
            return Err(LlvmInstallError::UnsupportedArchiveFormat {
                format: other.to_string(),
            });
        }
    };

    let output = match std::process::Command::new(tool).args(&arguments).output() {
        Ok(output) => output,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(LlvmInstallError::UnpackerUnavailable {
                tool,
                format: format.to_string(),
            });
        }
        Err(error) => return Err(InstallError::io("run the unpacker on", archive, error).into()),
    };
    if !output.status.success() {
        return Err(LlvmInstallError::ExtractFailed {
            archive: archive.to_path_buf(),
            detail: format!(
                "{tool} exited with {}: {}",
                output.status,
                String::from_utf8_lossy(&output.stderr).trim()
            ),
        });
    }
    Ok(())
}

/// Finds the LLVM tree inside an unpacked bundle.
///
/// The published bundles hold the install tree at their top level
/// (`tar -C <install> -cJf <archive> .`), and a bundle that wraps it in one
/// directory is accepted too. The test either way is the one discovery
/// applies, so nothing can be installed that discovery would then not find.
fn locate_bundle(unpacked: &Path) -> Result<PathBuf, LlvmInstallError> {
    if is_llvm_home(unpacked) {
        return Ok(unpacked.to_path_buf());
    }

    let entries =
        std::fs::read_dir(unpacked).map_err(|error| InstallError::io("read", unpacked, error))?;
    let mut directories = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| InstallError::io("read", unpacked, error))?;
        if entry.path().is_dir() {
            directories.push(entry.path());
        }
    }

    match directories.as_slice() {
        [only] if is_llvm_home(only) => Ok(only.clone()),
        _ => Err(LlvmInstallError::NotAnLlvmTree {
            unpacked: unpacked.to_path_buf(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derives_the_home_discovery_looks_in() {
        assert_eq!(
            llvm_home(Path::new("/tmp/knvm-root"), "22.1.4", "aarch64-macos"),
            Path::new("/tmp/knvm-root/llvm/22.1.4/aarch64-macos")
        );
    }

    /// The layout this installs into must be the layout `kira-toolchain`
    /// resolves for the user-level root, or a provisioned bundle would sit
    /// somewhere the backend's build script never looks.
    #[test]
    fn agrees_with_the_user_level_layout() {
        let Ok(user_level) = kira_toolchain::managed_llvm_home("22.1.4", "aarch64-macos") else {
            return; // no home directory in this environment
        };
        let Ok(root) = kira_toolchain::toolchains_root() else {
            return;
        };
        assert_eq!(llvm_home(&root, "22.1.4", "aarch64-macos"), user_level);
    }

    #[test]
    fn refuses_an_archive_format_it_cannot_unpack() {
        let temp = std::env::temp_dir().join(format!("knvm_llvmfmt_{}", std::process::id()));
        let error = extract(Path::new("/nonexistent.7z"), &temp, "7z")
            .expect_err("7z is not a format the pin may name");
        assert!(matches!(
            error,
            LlvmInstallError::UnsupportedArchiveFormat { .. }
        ));
        let _ = std::fs::remove_dir_all(&temp);
    }

    #[test]
    fn refuses_an_unpacked_tree_that_is_not_llvm() {
        let temp = std::env::temp_dir().join(format!("knvm_llvmtree_{}", std::process::id()));
        std::fs::create_dir_all(temp.join("bin")).expect("create a tree without the C API header");
        let error = locate_bundle(&temp).expect_err("a tree without `include/llvm-c/Core.h`");
        assert!(matches!(error, LlvmInstallError::NotAnLlvmTree { .. }));
        let _ = std::fs::remove_dir_all(&temp);
    }

    #[test]
    fn accepts_a_bundle_at_the_top_level_and_one_wrapped_in_a_directory() {
        let temp = std::env::temp_dir().join(format!("knvm_llvmfind_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&temp);

        let flat = temp.join("flat");
        std::fs::create_dir_all(flat.join("include").join("llvm-c")).expect("create flat tree");
        std::fs::write(flat.join("include").join("llvm-c").join("Core.h"), "header")
            .expect("write header");
        assert_eq!(locate_bundle(&flat).expect("a flat bundle is found"), flat);

        let wrapped = temp.join("wrapped");
        let inner = wrapped.join("llvm-22.1.4");
        std::fs::create_dir_all(inner.join("include").join("llvm-c")).expect("create wrapped tree");
        std::fs::write(
            inner.join("include").join("llvm-c").join("Core.h"),
            "header",
        )
        .expect("write header");
        assert_eq!(
            locate_bundle(&wrapped).expect("a wrapped bundle is found"),
            inner
        );

        let _ = std::fs::remove_dir_all(&temp);
    }
}
