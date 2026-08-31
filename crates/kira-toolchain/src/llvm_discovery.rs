//! Finding the LLVM install the native backend uses.
//!
//! Discovery is explicit and ordered — Kira never silently binds to whatever
//! LLVM happens to be on the system:
//!
//! 1. `KIRA_LLVM_HOME` (an explicit override always wins),
//! 2. the active managed install at
//!    `~/.kira/toolchains/llvm/<pinned-version>/<host-key>`,
//! 3. older repo-managed fallback paths under `<repo>/.kira/`, when a repo root
//!    is supplied and those trees already exist locally.
//!
//! When `llvm-config` exists inside the selected tree it refines the bin/lib
//! directories; otherwise the normal install layout is assumed. On failure the
//! error carries every path that was checked, so the caller can tell the user
//! exactly where Kira looked and what to do about it.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::llvm_layout::{
    current_host_llvm_bundle_key, legacy_llvm_current_home, legacy_llvm_versioned_home,
};
use crate::llvm_metadata::pinned_version;
use crate::{managed_llvm_clang_path, managed_llvm_home, managed_llvm_tool_path};

/// A resolved LLVM installation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LlvmInstallation {
    /// The install root (`KIRA_LLVM_HOME` or a managed/legacy tree).
    pub home: PathBuf,
    /// Where the tools live (refined by `llvm-config --bindir` when available).
    pub bin_dir: PathBuf,
    /// Where the libraries live (refined by `llvm-config --libdir`).
    pub lib_dir: PathBuf,
    /// The `llvm-config` in this tree, when it ships one.
    pub llvm_config: Option<PathBuf>,
    /// Which rule in the discovery order selected this tree.
    pub source: DiscoverySource,
}

impl LlvmInstallation {
    /// The `clang` driver in this installation — the linker driver Kira uses
    /// for native links.
    pub fn clang(&self) -> PathBuf {
        self.bin_dir.join(crate::executable_name("clang"))
    }

    /// The `llvm-ar` archiver in this installation.
    ///
    /// Used to build the static archive a Rust consumer of a Kira library links
    /// against. From the discovered install rather than `PATH` for the same
    /// reason [`LlvmInstallation::clang`] is: a host's `ar` may not understand
    /// the MRI script that merges the runtime archive in, and a toolchain that
    /// picked its tools up off `PATH` would work on one machine.
    pub fn llvm_ar(&self) -> PathBuf {
        self.bin_dir.join(crate::executable_name("llvm-ar"))
    }

    /// The Address Sanitizer runtime this installation ships for a target
    /// operating system, or an error naming exactly what is missing.
    ///
    /// From this tree and nowhere else. A host's Xcode or distro clang carries
    /// its own ASan runtime, and binding to it would sanitize with one
    /// compiler's contract what another compiler instrumented — the rule that
    /// keeps discovery off `PATH` keeps it out of `/Applications` too. A
    /// bundle built before compiler-rt was added answers with the path it
    /// looked at, which is the fact the user needs to update the bundle.
    pub fn asan_runtime(&self, target_os: AsanTargetOs) -> Result<PathBuf, LlvmDiscoveryError> {
        let (clang_lib, resource) = self.clang_resource_dir()?;
        let candidates: Vec<PathBuf> = match target_os {
            // Every Apple platform's runtime lives in one `darwin` directory,
            // distinguished by the platform infix Apple's own driver uses.
            AsanTargetOs::Apple { platform } => {
                vec![resource.join("lib").join("darwin").join(format!(
                    "libclang_rt.asan_{}_dynamic.dylib",
                    platform.runtime_infix()
                ))]
            }
            AsanTargetOs::LinuxGnu { triple } => vec![
                // Per-target runtime directory, the layout a runtimes build
                // installs by default.
                resource
                    .join("lib")
                    .join(&triple)
                    .join("libclang_rt.asan.a"),
                // The flat legacy layout, for a bundle configured without it.
                resource.join("lib").join("linux").join(format!(
                    "libclang_rt.asan-{}.a",
                    triple.split('-').next().unwrap_or(triple.as_str())
                )),
            ],
            AsanTargetOs::WindowsMsvc { arch } => vec![
                resource
                    .join("lib")
                    .join("windows")
                    .join(format!("clang_rt.asan_dynamic-{arch}.lib")),
            ],
            AsanTargetOs::Android { triple } => vec![
                // Per-target runtime directory, as for linux-gnu.
                resource
                    .join("lib")
                    .join(&triple)
                    .join("libclang_rt.asan.so"),
                // The flat NDK-style layout files Android under `linux` with
                // an `-android` suffix on the architecture.
                resource.join("lib").join("linux").join(format!(
                    "libclang_rt.asan-{}-android.so",
                    triple.split('-').next().unwrap_or(triple.as_str())
                )),
            ],
        };
        candidates
            .iter()
            .find(|path| path.is_file())
            .cloned()
            .ok_or_else(|| LlvmDiscoveryError::AsanRuntimeMissing {
                looked_at: candidates
                    .first()
                    .cloned()
                    .unwrap_or_else(|| clang_lib.clone()),
            })
    }

    /// The dynamic AddressSanitizer runtime a host must load before a sanitized
    /// hybrid library, or an error naming exactly what the bundle lacks.
    pub fn asan_preload_runtime(
        &self,
        target_os: AsanTargetOs,
    ) -> Result<PathBuf, LlvmDiscoveryError> {
        let (clang_lib, resource) = self.clang_resource_dir()?;
        let candidates: Vec<PathBuf> = match target_os {
            AsanTargetOs::Apple { platform } => {
                vec![resource.join("lib").join("darwin").join(format!(
                    "libclang_rt.asan_{}_dynamic.dylib",
                    platform.runtime_infix()
                ))]
            }
            AsanTargetOs::LinuxGnu { triple } => vec![
                resource
                    .join("lib")
                    .join(&triple)
                    .join("libclang_rt.asan.so"),
                resource.join("lib").join("linux").join(format!(
                    "libclang_rt.asan-{}.so",
                    triple.split('-').next().unwrap_or(triple.as_str())
                )),
            ],
            AsanTargetOs::WindowsMsvc { arch } => vec![
                resource
                    .join("lib")
                    .join("windows")
                    .join(format!("clang_rt.asan_dynamic-{arch}.dll")),
            ],
            AsanTargetOs::Android { triple } => vec![
                resource
                    .join("lib")
                    .join(&triple)
                    .join("libclang_rt.asan.so"),
                resource.join("lib").join("linux").join(format!(
                    "libclang_rt.asan-{}-android.so",
                    triple.split('-').next().unwrap_or(triple.as_str())
                )),
            ],
        };
        candidates
            .iter()
            .find(|path| path.is_file())
            .cloned()
            .ok_or_else(|| LlvmDiscoveryError::AsanRuntimeMissing {
                looked_at: candidates
                    .first()
                    .cloned()
                    .unwrap_or_else(|| clang_lib.clone()),
            })
    }

    fn clang_resource_dir(&self) -> Result<(PathBuf, PathBuf), LlvmDiscoveryError> {
        let clang_lib = self.home.join("lib").join("clang");
        let resource = std::fs::read_dir(&clang_lib)
            .ok()
            .and_then(|entries| {
                entries
                    .filter_map(|entry| entry.ok())
                    .map(|entry| entry.path())
                    .find(|path| path.is_dir())
            })
            .ok_or_else(|| LlvmDiscoveryError::AsanRuntimeMissing {
                looked_at: clang_lib.clone(),
            })?;
        Ok((clang_lib, resource))
    }
}

/// The operating systems `--sanitize address` can target.
///
/// A closed list rather than a triple string, so a platform the runtime lookup
/// has no layout for is refused at the call rather than answered with a path
/// that cannot exist. The list covers every platform Kira emits native code
/// for; what a given bundle actually *ships* is answered by the lookup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AsanTargetOs {
    /// An Apple platform build, served by its darwin dynamic runtime.
    Apple {
        /// Which Apple platform, which names the runtime file.
        platform: ApplePlatform,
    },
    /// A `*-linux-gnu` build, served by the static runtime archive.
    LinuxGnu {
        /// The full target triple, which names the per-target runtime dir.
        triple: String,
    },
    /// An `*-windows-msvc` build, served by the dynamic thunk import library
    /// (the runtime DLL travels beside it).
    WindowsMsvc {
        /// The architecture infix in compiler-rt's Windows file names.
        arch: String,
    },
    /// An `*-linux-android` build, served by the dynamic runtime the app
    /// bundles beside its own libraries.
    Android {
        /// The full target triple, which names the per-target runtime dir.
        triple: String,
    },
}

/// The Apple platforms Kira builds for, spelled the way compiler-rt names
/// their runtime slices.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplePlatform {
    /// macOS.
    Macos,
    /// iOS on device.
    Ios,
    /// The iOS simulator.
    IosSimulator,
    /// tvOS on device.
    Tvos,
    /// The tvOS simulator.
    TvosSimulator,
    /// visionOS on device.
    Visionos,
    /// The visionOS simulator.
    VisionosSimulator,
}

impl ApplePlatform {
    /// The infix in `libclang_rt.asan_<infix>_dynamic.dylib`.
    pub fn runtime_infix(self) -> &'static str {
        match self {
            ApplePlatform::Macos => "osx",
            ApplePlatform::Ios => "ios",
            ApplePlatform::IosSimulator => "iossim",
            ApplePlatform::Tvos => "tvos",
            ApplePlatform::TvosSimulator => "tvossim",
            ApplePlatform::Visionos => "xros",
            ApplePlatform::VisionosSimulator => "xrossim",
        }
    }
}

/// Which discovery rule produced an [`LlvmInstallation`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiscoverySource {
    /// The `KIRA_LLVM_HOME` override.
    EnvironmentOverride,
    /// The managed install under `~/.kira/toolchains/llvm/`.
    ManagedInstall,
    /// An older repo-managed tree under `<repo>/.kira/`.
    LegacyRepoInstall,
}

/// Why LLVM discovery failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LlvmDiscoveryError {
    /// The installed LLVM bundle carries no Address Sanitizer runtime.
    #[error(
        "the installed LLVM bundle has no Address Sanitizer runtime (looked at \
         `{looked_at}`); `--sanitize address` links the runtime from the managed \
         bundle and never from a host compiler, so update the pinned LLVM bundle \
         to one built with compiler-rt"
    )]
    AsanRuntimeMissing {
        /// The path inside the bundle where the runtime was expected.
        looked_at: PathBuf,
    },
    /// `KIRA_LLVM_HOME` was set but does not name a usable LLVM tree.
    #[error(
        "KIRA_LLVM_HOME is set to `{path}` but that is not a usable LLVM install \
         (no `include/llvm-c/Core.h`); point it at an LLVM install root or unset it \
         to use the managed toolchain"
    )]
    OverrideUnusable {
        /// The path `KIRA_LLVM_HOME` named.
        path: PathBuf,
    },
    /// This host has no published managed bundle.
    #[error(
        "no managed LLVM bundle is published for this host ({os}/{arch}); \
         set KIRA_LLVM_HOME to an LLVM {version} install root"
    )]
    UnsupportedHost {
        /// The host OS (`std::env::consts::OS`).
        os: String,
        /// The host architecture (`std::env::consts::ARCH`).
        arch: String,
        /// The pinned LLVM version.
        version: String,
    },
    /// The compiled-in LLVM pin could not be read.
    #[error(transparent)]
    Metadata(#[from] crate::llvm_metadata::MalformedMetadata),
    /// Nothing was found anywhere in the discovery order.
    // Read at the one moment there is no LLVM, which is also the moment `kira`
    // cannot be built — it links the backend this discovery feeds. So the route
    // named here is knvm's: it links no LLVM, so it builds from a bare checkout
    // and is the only one of the two that exists before the bundle does.
    #[error(
        "no LLVM {version} install found; checked:\n{}\n\
         install the pinned bundle with `knvm install-llvm`, or from a checkout \
         with `cargo run -p kira-knvm -- install-llvm`; or set KIRA_LLVM_HOME to \
         an LLVM {version} install root",
        .checked.iter().map(|path| format!("  {}", path.display())).collect::<Vec<_>>().join("\n")
    )]
    NotFound {
        /// The pinned LLVM version.
        version: String,
        /// Every path that was checked, in discovery order.
        checked: Vec<PathBuf>,
    },
}

/// Resolves the LLVM installation to build against.
///
/// `repo_root` enables the legacy repo-managed fallback; pass `None` outside a
/// repo checkout (an installed toolchain).
pub fn discover(repo_root: Option<&Path>) -> Result<LlvmInstallation, LlvmDiscoveryError> {
    let version = pinned_version()?;
    let mut checked = Vec::new();

    // 1. An explicit override always wins, and never falls through: if the user
    //    named a tree and it is wrong, say so rather than silently using another.
    if let Some(home) = env_override() {
        return if is_llvm_home(&home) {
            Ok(describe(home, DiscoverySource::EnvironmentOverride))
        } else {
            Err(LlvmDiscoveryError::OverrideUnusable { path: home })
        };
    }

    // 2. The active managed install for this host.
    let Some(host_key) = current_host_llvm_bundle_key() else {
        return Err(LlvmDiscoveryError::UnsupportedHost {
            os: std::env::consts::OS.to_owned(),
            arch: std::env::consts::ARCH.to_owned(),
            version: version.to_owned(),
        });
    };
    if let Ok(home) = managed_llvm_home(version, host_key) {
        if is_llvm_home(&home) {
            return Ok(describe(home, DiscoverySource::ManagedInstall));
        }
        checked.push(home);
    }

    // 3. Older repo-managed trees, only when they already exist locally.
    if let Some(root) = repo_root {
        for home in [
            legacy_llvm_current_home(root),
            legacy_llvm_versioned_home(root, version, host_key),
        ] {
            if is_llvm_home(&home) {
                return Ok(describe(home, DiscoverySource::LegacyRepoInstall));
            }
            checked.push(home);
        }
    }

    Err(LlvmDiscoveryError::NotFound {
        version: version.to_owned(),
        checked,
    })
}

/// The `KIRA_LLVM_HOME` override, when set to a non-empty value.
fn env_override() -> Option<PathBuf> {
    let value = std::env::var_os("KIRA_LLVM_HOME")?;
    (!value.is_empty()).then(|| PathBuf::from(value))
}

/// Whether `home` looks like an LLVM install tree.
///
/// The C API header is the load-bearing artifact: it is what the backend's
/// bindings are generated against, and every layout Kira accepts ships it.
///
/// Public because whatever *installs* a bundle must accept exactly what
/// discovery will later look for. A provisioner with its own idea of a
/// complete tree is how an install succeeds and the next build reports that
/// nothing is installed.
#[must_use]
pub fn is_llvm_home(home: &Path) -> bool {
    home.join("include").join("llvm-c").join("Core.h").is_file()
}

/// Builds an [`LlvmInstallation`], refining directories through `llvm-config`
/// when the tree ships one.
fn describe(home: PathBuf, source: DiscoverySource) -> LlvmInstallation {
    let config_path = managed_llvm_tool_path(&home, "llvm-config");
    let llvm_config = config_path.is_file().then_some(config_path);

    let bin_dir = llvm_config
        .as_deref()
        .and_then(|config| query(config, "--bindir"))
        .unwrap_or_else(|| home.join("bin"));
    let lib_dir = llvm_config
        .as_deref()
        .and_then(|config| query(config, "--libdir"))
        .unwrap_or_else(|| home.join("lib"));

    LlvmInstallation {
        home,
        bin_dir,
        lib_dir,
        llvm_config,
        source,
    }
}

/// Asks `llvm-config` for a directory, or `None` when it cannot be run.
fn query(llvm_config: &Path, flag: &str) -> Option<PathBuf> {
    let output = Command::new(llvm_config).arg(flag).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?;
    let trimmed = text.trim();
    (!trimmed.is_empty()).then(|| PathBuf::from(trimmed))
}

/// The `clang` Kira would use from a managed LLVM home, for callers that only
/// have the home path.
pub fn clang_in(home: &Path) -> PathBuf {
    managed_llvm_clang_path(home)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_directory_without_the_c_api_header_is_not_an_llvm_home() {
        assert!(!is_llvm_home(Path::new("/definitely/not/llvm")));
    }

    #[test]
    fn the_error_lists_every_checked_path_and_the_remedy() {
        let error = LlvmDiscoveryError::NotFound {
            version: "22.1.4".to_owned(),
            checked: vec![PathBuf::from("/a/one"), PathBuf::from("/b/two")],
        };
        let text = error.to_string();
        assert!(text.contains("/a/one") && text.contains("/b/two"));
        assert!(text.contains("KIRA_LLVM_HOME"), "{text}");
        // The remedy has to be buildable on a machine with no LLVM, which
        // `kira` is not: it links the backend that this discovery resolves.
        assert!(text.contains("knvm install-llvm"), "{text}");
        assert!(
            !text.contains("kira fetch-llvm"),
            "the remedy must not be a `kira` that cannot be built yet: {text}"
        );
    }
}
