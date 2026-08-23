//! Provisions the managed LLVM bundle for `knvm install-llvm`.
//!
//! The bundle is stored at `<toolchains-root>/llvm/<version>/<host-key>`, the
//! path discovered by the LLVM backend. The compiled `llvm-metadata.toml` pin
//! supplies the version, release asset, and host-specific filename.
//!
//! LLVM is a versioned sibling of installed toolchain directories, so this verb
//! updates only the `llvm/` tree and can serve every installed toolchain that
//! uses the same LLVM version.

use std::path::{Path, PathBuf};

use kira_toolchain::{TargetBundle, is_llvm_home};

use crate::digest::{Sha256, checksum_file_name};
use crate::github;
use crate::install::{InstallError, Staging};
use crate::source::{ReleaseSourceError, read_published_checksum};
use crate::unpack::{self, UnpackError};

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
    /// No unpacking tool for the format is on this host.
    #[error(
        "none of {} were found on PATH; knvm unpacks `{format}` bundles with one of them",
        .tools.join(", ")
    )]
    UnpackerUnavailable {
        /// The tools that were looked for, in the order they were tried.
        tools: &'static [&'static str],
        /// The format they would have unpacked.
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
    /// The pinned code generators this bundle does not carry.
    ///
    /// Empty for a bundle that matches the pin. A release owns its assets for
    /// good, so a pin that grows a code generator after its release was cut
    /// installs a real LLVM that is nonetheless short of one — and a compiler
    /// built against it refuses that device by name. Reporting it here is what
    /// makes that visible while the bundle is being installed, rather than at
    /// the first build that wanted the device.
    pub missing_code_generators: Vec<String>,
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
            let missing_code_generators = missing_code_generators(&home);
            return Ok(LlvmInstalled {
                version: pin.llvm.version.clone(),
                host_key,
                home,
                already_installed: true,
                verified: None,
                missing_code_generators,
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

    let missing_code_generators = missing_code_generators(&home);
    Ok(LlvmInstalled {
        version: pin.llvm.version.clone(),
        host_key,
        home,
        already_installed: false,
        verified,
        missing_code_generators,
    })
}

/// The pinned code generators the bundle at `home` does not carry.
///
/// A bundle that cannot be asked — no `llvm-config`, or one that refuses to
/// run — reports nothing missing rather than everything: this is a report on
/// an install that otherwise succeeded, and the backend's build script asks the
/// same question again and fails the build there when the answer is unreadable.
fn missing_code_generators(home: &Path) -> Vec<String> {
    kira_toolchain::llvm_code_generators::missing_from(home)
        .unwrap_or_default()
        .into_iter()
        .map(str::to_owned)
        .collect()
}

/// The pinned code generators the LLVM a build in `directory` would link does
/// not carry.
///
/// Discovery is the one the backend's build script runs, so the answer is
/// about the exact bundle that build links — a `KIRA_LLVM_HOME` override
/// included. A machine with no discoverable LLVM reports nothing missing: it
/// is short of the whole bundle, which the build refuses by itself.
#[must_use]
pub fn missing_code_generators_for_build(directory: &Path) -> Vec<String> {
    match kira_toolchain::llvm_discovery::discover(Some(directory)) {
        Ok(installation) => missing_code_generators(&installation.home),
        Err(_) => Vec::new(),
    }
}

/// What a bundle missing `missing` means for the compiler built against it.
///
/// One line per generator, because "the Web device is unavailable" is the part
/// that matters and the generator's LLVM name alone does not say it.
#[must_use]
pub fn code_generator_shortfall(missing: &[String], release_tag: &str) -> Vec<String> {
    missing
        .iter()
        .map(|name| {
            let consequence = if name == kira_toolchain::WEB_CODE_GENERATOR {
                "a kira built against it refuses `--device wasm32`"
            } else {
                "a kira built against it refuses `--target` triples for that architecture"
            };
            format!(
                "the bundle published under `{release_tag}` carries no {name} code \
                 generator, which `llvm-metadata.toml` pins: {consequence}"
            )
        })
        .collect()
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
/// The mechanics are shared with everything else knvm installs; what is local
/// here is only how a refusal is reported, because these variants are part of
/// this verb's error surface and its callers match on them.
fn extract(archive: &Path, destination: &Path, format: &str) -> Result<(), LlvmInstallError> {
    unpack::extract(archive, destination, format).map_err(|error| match error {
        UnpackError::UnsupportedFormat { format } => {
            LlvmInstallError::UnsupportedArchiveFormat { format }
        }
        UnpackError::UnpackerUnavailable { tools, format } => {
            LlvmInstallError::UnpackerUnavailable { tools, format }
        }
        UnpackError::Failed { archive, detail } => {
            LlvmInstallError::ExtractFailed { archive, detail }
        }
        UnpackError::Io(error) => error.into(),
    })
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
    use crate::unpack::{unpack_arguments, unpackers};

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

    /// The Windows bundle is the only zip the pin names, and no one tool
    /// unpacks it on every Windows shell: a stock PowerShell has no `unzip`,
    /// and an MSYS shell's `tar` refuses a zip. Naming only one of them makes
    /// `install-llvm` unusable on one of the two.
    #[test]
    fn a_zip_names_both_windows_unpackers() {
        let tools = unpackers("zip").expect("the pin may name `zip`");
        assert!(tools.contains(&"unzip"), "{tools:?}");
        assert!(tools.contains(&"tar"), "{tools:?}");
    }

    #[test]
    fn a_tarball_is_unpacked_by_tar_and_an_unknown_format_by_nothing() {
        assert_eq!(unpackers("tar.xz"), Some(&["tar"][..]));
        assert!(unpackers("7z").is_none());
    }

    /// `unzip` names its destination with `-d`; everything else here is `tar`,
    /// which names it with `-C`. Handing one the other's flag unpacks nothing.
    #[test]
    fn each_unpacker_is_given_its_own_destination_flag() {
        let archive = Path::new("/bundle.zip");
        let destination = Path::new("/dest");
        assert!(
            unpack_arguments("unzip", archive, destination).contains(&std::ffi::OsStr::new("-d"))
        );
        assert!(
            unpack_arguments("tar", archive, destination).contains(&std::ffi::OsStr::new("-C"))
        );
    }

    /// A bundle that carries the pin reports nothing, so the common install
    /// says nothing about code generators at all.
    #[test]
    fn a_bundle_matching_the_pin_has_nothing_to_report() {
        assert!(code_generator_shortfall(&[], "llvm-v22.1.4-kira.1").is_empty());
    }

    /// The Web generator's absence is reported as the device it costs, because
    /// `WebAssembly` is an LLVM target name and `--device wasm32` is the thing
    /// the reader loses.
    #[test]
    fn the_web_generators_absence_names_the_device_it_costs() {
        let lines = code_generator_shortfall(
            &[kira_toolchain::WEB_CODE_GENERATOR.to_owned()],
            "llvm-v22.1.4-kira.1",
        );
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("llvm-v22.1.4-kira.1"), "{lines:?}");
        assert!(
            lines[0].contains(kira_toolchain::WEB_CODE_GENERATOR),
            "{lines:?}"
        );
        assert!(lines[0].contains("--device wasm32"), "{lines:?}");
    }

    /// An architecture generator is not the Web one, so it gets the consequence
    /// that fits it rather than a line about a device it has nothing to do with.
    #[test]
    fn an_architecture_generator_reports_the_emission_it_costs() {
        let lines = code_generator_shortfall(&["AArch64".to_owned()], "llvm-v22.1.4-kira.1");
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("AArch64"), "{lines:?}");
        assert!(!lines[0].contains("wasm32"), "{lines:?}");
        assert!(
            lines[0].contains("refuses `--target` triples for that architecture"),
            "{lines:?}"
        );
    }

    /// One line per generator: a bundle short of two is short of two things,
    /// and a reader fixing it needs both named.
    #[test]
    fn every_missing_generator_gets_its_own_line() {
        let lines = code_generator_shortfall(
            &[
                kira_toolchain::WEB_CODE_GENERATOR.to_owned(),
                "AArch64".to_owned(),
            ],
            "llvm-v22.1.4-kira.1",
        );
        assert_eq!(lines.len(), 2);
    }
}
