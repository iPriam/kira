//! Building this checkout into an installed dev toolchain.
//!
//! `knvm binstall` is the developer's install route: it compiles the compiler
//! out of the checkout it is run inside, shapes the result into the same tree a
//! release archive unpacks to — `bin/kira` with `foundation/` beside it — and
//! lands it on the `dev` channel through the same staging/validate/rename
//! pipeline a release install uses. Installing selects it, so `kira` dispatches
//! to the fresh build immediately.
//!
//! It builds OPTIMIZED by default. A dev toolchain is what every project in the
//! tree then compiles through, and an unoptimized compiler turns a UI library's
//! build from one minute into eight — the cost lands on every downstream build,
//! all day, not on the one command that produced it. `--debug` stages the
//! unoptimized build for the case that wants it: debugging the compiler itself.
//!
//! Running it again replaces the installed tree. A dev toolchain names a
//! moving target, so "already installed" would mean "silently stale" — the
//! one thing a rebuild command must never be.

use std::path::{Path, PathBuf};
use std::process::Command;

use kira_toolchain::{
    Channel, CurrentToolchain, DESKTOP_RUNNER_BINARY, HYBRID_LAUNCHER_BINARY,
    LANGUAGE_SERVER_BINARY, executable_name, static_archive_name,
};

use crate::install::{
    InstallError, Installed, PRIMARY_BINARY, Staging, toolchain_root, validate, write_current,
};

/// The manifest that marks the bundled Foundation as a real Kira package.
const PACKAGE_MANIFEST_FILE_NAME: &str = "package.kira";

/// Which cargo profile a dev toolchain is built with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BuildProfile {
    /// Optimized, the way an installed toolchain is shipped.
    #[default]
    Release,
    /// Unoptimized and with debug info, for working on the compiler itself.
    Debug,
}

impl BuildProfile {
    /// The cargo flags this profile adds to `cargo build`.
    fn cargo_flags(self) -> &'static [&'static str] {
        match self {
            BuildProfile::Release => &["--release"],
            BuildProfile::Debug => &[],
        }
    }

    /// The directory under `target/` cargo writes this profile's artifacts to.
    fn target_subdirectory(self) -> &'static str {
        match self {
            BuildProfile::Release => "release",
            BuildProfile::Debug => "debug",
        }
    }
}

/// Why a checkout could not be built into an installed toolchain.
#[derive(Debug, thiserror::Error)]
pub enum BinstallError {
    /// No enclosing directory carries the checkout markers.
    #[error(
        "`{}` is not inside a Kira checkout: no enclosing directory holds \
         `Cargo.toml` beside `foundation/{PACKAGE_MANIFEST_FILE_NAME}`",
        .start.display()
    )]
    NotACheckout {
        /// Where the search began.
        start: PathBuf,
    },
    /// The checkout's workspace manifest does not state a version.
    #[error(
        "`{}` does not state `[workspace.package] version`, so the dev \
         toolchain cannot be named",
        .manifest.display()
    )]
    VersionUnreadable {
        /// The manifest that was read.
        manifest: PathBuf,
    },
    /// `cargo` is required to build the checkout and is not on PATH.
    #[error("`cargo` was not found on PATH; binstall builds the checkout with it")]
    CargoUnavailable,
    /// No managed LLVM was found, and kira cannot be built without one.
    #[error(
        "no managed LLVM found under the toolchains root; the LLVM backend is \
         part of every kira build. Provision one (see `llvm-metadata.toml` \
         for the pinned version) and try again"
    )]
    LlvmMissing,
    /// The build ran and failed.
    #[error("`cargo build -p kira-cli` failed in `{}`; its output names the error", .checkout.display())]
    BuildFailed {
        /// The checkout the build ran in.
        checkout: PathBuf,
    },
    /// The build succeeded but the expected binary is not where cargo puts it.
    #[error("the build left no compiler at `{}`", .expected.display())]
    MissingBuildArtifact {
        /// Where the binary was expected.
        expected: PathBuf,
    },
    /// The user's persistent environment could not be read (Windows).
    #[error("could not read this user's persistent `Path`: {detail}")]
    UserPathUnreadable {
        /// What the read reported.
        detail: String,
    },
    /// The user's persistent environment could not be written (Windows).
    #[error("could not add the kira tools to this user's persistent `Path`: {detail}")]
    UserPathUnwritable {
        /// What the write reported.
        detail: String,
    },
    /// Staging, validating, or landing the tree failed.
    #[error(transparent)]
    Install(#[from] InstallError),
    /// The new toolchain failed to land *and* the previous one could not be
    /// put back.
    ///
    /// Both copies are named: the previous build still exists under staging,
    /// and the remedy is moving it back by hand — losing it silently would
    /// turn a failed upgrade into a wiped toolchain.
    #[error(
        "the built toolchain could not be moved into `{destination}` \
         ({source}) and the previous build could not be restored from \
         `{retired}` ({restore}); move `{retired}` back to \
         `{destination}` to recover the old toolchain"
    )]
    SwapLostPrevious {
        /// Where the new toolchain was being landed.
        destination: PathBuf,
        /// Where the previous build waits inside staging.
        retired: PathBuf,
        /// Why landing the new build failed.
        source: Box<InstallError>,
        /// Why restoring the old one failed.
        restore: Box<InstallError>,
    },
}

/// The packages `binstall` builds before it stages a toolchain.
///
/// Both runtime crates are named even though `kira-cli` depends on them,
/// because those dependencies build their **rlibs** and a toolchain installs
/// their **staticlibs** — cargo builds a crate-type nobody asked for only when
/// the crate itself is a target. Leaving either out installs a fresh compiler
/// beside an absent or stale archive: ordinary native programs need the base
/// bridge, while programs that call the compiler need its superset.
///
/// The desktop runner is here for the same reason and a different one: nothing
/// depends on it at all, so only naming it builds it, and `kira live` starts it
/// beside the compiler — a toolchain without it builds a bundle and then has
/// nowhere to run it. The hybrid launcher is its twin: a hybrid build stages it
/// beside the compiler as the program's standalone executable.
const BUILD_PACKAGES: [&str; 7] = [
    "kira-cli",
    "kira-lsp",
    "kira-desktop-runner",
    "kira-hybrid-launcher",
    "kira-native-bridge",
    "kira-compiler-bridge",
    "kira-libffi",
];

/// Builds the enclosing checkout and installs it as the selected dev toolchain.
///
/// `start` is where the checkout search begins — the working directory, for the
/// binary. The build's stdout and stderr are inherited, so a compile error
/// lands in front of the user rather than in a captured buffer.
pub fn binstall(
    toolchains_root: &Path,
    start: &Path,
    profile: BuildProfile,
) -> Result<Installed, BinstallError> {
    let checkout = enclosing_checkout(start).ok_or_else(|| BinstallError::NotACheckout {
        start: start.to_path_buf(),
    })?;
    let version = workspace_version(&checkout)?;

    // The LLVM backend is a hard dependency of kira, and its build script
    // discovers the managed bundle itself; this check only refuses up front,
    // with the provisioning route named, rather than three crates into a
    // build that cannot finish.
    if kira_toolchain::llvm_discovery::discover(Some(&checkout)).is_err() {
        return Err(BinstallError::LlvmMissing);
    }
    let mut build = Command::new("cargo");
    build.arg("build");
    build.args(profile.cargo_flags());
    for package in BUILD_PACKAGES {
        build.args(["-p", package]);
    }
    build.current_dir(&checkout);
    let built = build.status().map_err(|error| match error.kind() {
        std::io::ErrorKind::NotFound => BinstallError::CargoUnavailable,
        _ => BinstallError::BuildFailed {
            checkout: checkout.clone(),
        },
    })?;
    if !built.success() {
        return Err(BinstallError::BuildFailed { checkout });
    }

    // The same archive again, cross-built for emscripten, since a Web build
    // links against a bridge compiled for wasm32 rather than the host's.
    let mut cross_build = Command::new("cargo");
    cross_build.args([
        "build",
        "-p",
        "kira-native-bridge",
        "--target",
        "wasm32-unknown-emscripten",
    ]);
    cross_build.args(profile.cargo_flags());
    let cross = cross_build
        .current_dir(&checkout)
        .status()
        .map_err(|error| match error.kind() {
            std::io::ErrorKind::NotFound => BinstallError::CargoUnavailable,
            _ => BinstallError::BuildFailed {
                checkout: checkout.clone(),
            },
        })?;
    if !cross.success() {
        return Err(BinstallError::BuildFailed { checkout });
    }

    let built_dir = target_dir(&checkout).join(profile.target_subdirectory());
    let compiler = built_dir.join(executable_name(PRIMARY_BINARY));
    let language_server = built_dir.join(executable_name(LANGUAGE_SERVER_BINARY));
    let desktop_runner = built_dir.join(executable_name(DESKTOP_RUNNER_BINARY));
    let hybrid_launcher = built_dir.join(executable_name(HYBRID_LAUNCHER_BINARY));
    // Under the names cargo wrote them: `<name>.lib` under MSVC. The Web
    // archive keeps its Unix spelling on every host, because emscripten wrote
    // that one rather than the host toolchain.
    let host_archive = built_dir.join(static_archive_name("kira_native_bridge"));
    let compiler_archive = built_dir.join(static_archive_name("kira_compiler_bridge"));
    // Every native artifact links the libffi helper, which carries libffi
    // itself: the engine is linked in rather than shipped beside the artifact,
    // so there is no separate binary for a toolchain to stage.
    let libffi_archive = built_dir.join(static_archive_name("kira_libffi"));
    let wasm_archive = target_dir(&checkout)
        .join("wasm32-unknown-emscripten")
        .join(profile.target_subdirectory())
        .join("libkira_native_bridge.a");
    for artifact in [
        &compiler,
        &language_server,
        &desktop_runner,
        &hybrid_launcher,
        &host_archive,
        &compiler_archive,
        &libffi_archive,
        &wasm_archive,
    ] {
        if !artifact.is_file() {
            return Err(BinstallError::MissingBuildArtifact {
                expected: artifact.clone(),
            });
        }
    }

    // The same discipline as a release install: shape and validate the whole
    // tree in staging, then swap it into place. The previous build is set
    // aside inside staging and restored by hand if the swap fails, so a
    // failure at the swap point can cost neither the new build nor the old.
    let staging = Staging::create(toolchains_root)?;
    let payload = staging.path().join(format!("kira-{version}"));
    let bin = payload.join("bin");
    create_dir(&bin)?;
    let staged_compiler = bin.join(executable_name(PRIMARY_BINARY));
    std::fs::copy(&compiler, &staged_compiler)
        .map_err(|error| InstallError::io("copy the compiler to", &staged_compiler, error))?;
    // The language server ships with the toolchain so an editor always runs
    // the selected compiler's frontend — a server installed separately goes
    // stale the moment the language moves.
    let staged_server = bin.join(executable_name(LANGUAGE_SERVER_BINARY));
    std::fs::copy(&language_server, &staged_server)
        .map_err(|error| InstallError::io("copy the language server to", &staged_server, error))?;
    // `kira live` starts the runner client from beside itself, so the runner
    // belongs to the toolchain rather than to whatever checkout happened to
    // build one: a session that picked a runner off PATH would run a bundle on
    // a client from a different build.
    let staged_runner = bin.join(executable_name(DESKTOP_RUNNER_BINARY));
    std::fs::copy(&desktop_runner, &staged_runner)
        .map_err(|error| InstallError::io("copy the desktop runner to", &staged_runner, error))?;
    // A hybrid build stages the launcher from beside itself as the program's
    // standalone executable, so for the same reason it belongs to the
    // toolchain: a compiler without it can build a hybrid bundle and has
    // nothing to stage as the program.
    let staged_launcher = bin.join(executable_name(HYBRID_LAUNCHER_BINARY));
    std::fs::copy(&hybrid_launcher, &staged_launcher).map_err(|error| {
        InstallError::io("copy the hybrid launcher to", &staged_launcher, error)
    })?;
    // The debug information beside each executable, where a profiler and a
    // debugger look for it.
    //
    // Without it a sampled profile of a VM run resolves only the *exported*
    // symbols of the compiler — a handful of `kira_rt_*` and the debugger's own
    // probe — and attributes the whole interpreter to whichever of those
    // happens to precede it in the image. That is not a degraded profile, it is
    // a wrong one: it named `kira_vm_debug_probe` as half of an editor frame in
    // a run that never entered the debugger.
    for (built, staged) in [
        (&compiler, &staged_compiler),
        (&language_server, &staged_server),
        (&desktop_runner, &staged_runner),
        (&hybrid_launcher, &staged_launcher),
    ] {
        copy_debug_info(built, staged)?;
    }
    // The runtime archives ride beside the compiler, where its archive
    // resolution looks: the host's under its cargo name, the Web's under a
    // target-suffixed one so neither can be linked in the other's place.
    let staged_host = bin.join(static_archive_name("kira_native_bridge"));
    std::fs::copy(&host_archive, &staged_host)
        .map_err(|error| InstallError::io("copy the runtime archive to", &staged_host, error))?;
    let staged_compiler = bin.join(static_archive_name("kira_compiler_bridge"));
    std::fs::copy(&compiler_archive, &staged_compiler).map_err(|error| {
        InstallError::io(
            "copy the compiler runtime archive to",
            &staged_compiler,
            error,
        )
    })?;
    let staged_libffi_archive = bin.join(static_archive_name("kira_libffi"));
    std::fs::copy(&libffi_archive, &staged_libffi_archive).map_err(|error| {
        InstallError::io(
            "copy the libffi helper archive to",
            &staged_libffi_archive,
            error,
        )
    })?;
    let staged_wasm = bin.join("libkira_native_bridge-wasm32-emscripten.a");
    std::fs::copy(&wasm_archive, &staged_wasm).map_err(|error| {
        InstallError::io("copy the Web runtime archive to", &staged_wasm, error)
    })?;
    copy_tree(&checkout.join("foundation"), &payload.join("foundation"))?;
    validate(&payload)?;

    let destination = toolchain_root(toolchains_root, Channel::Dev, &version);
    if let Some(parent) = destination.parent() {
        create_dir(parent)?;
    }
    let replaced = destination.is_dir();
    let retired = staging.path().join("replaced");
    if replaced {
        std::fs::rename(&destination, &retired).map_err(|error| {
            InstallError::io("set aside the previous build at", &destination, error)
        })?;
    }
    // The previous build is set aside *inside* staging, whose guard deletes
    // everything under it on the error paths — so a failed final swap would
    // take the only copy of it down with the rest. Before returning that
    // failure, put it back: a binstall that replaces nothing must not also
    // destroy what was already there.
    if let Err(error) = std::fs::rename(&payload, &destination) {
        if replaced {
            let restored = std::fs::rename(&retired, &destination);
            match restored {
                Ok(()) => {
                    return Err(InstallError::io(
                        "move the built toolchain into",
                        &destination,
                        error,
                    )
                    .into());
                }
                Err(restore) => {
                    return Err(BinstallError::SwapLostPrevious {
                        destination: destination.clone(),
                        retired: retired.clone(),
                        source: Box::new(InstallError::io(
                            "move the built toolchain into",
                            &destination,
                            error,
                        )),
                        restore: Box::new(InstallError::io(
                            "restore the previous build from",
                            &retired,
                            restore,
                        )),
                    });
                }
            }
        }
        return Err(InstallError::io("move the built toolchain into", &destination, error).into());
    }

    write_current(
        toolchains_root,
        &CurrentToolchain {
            channel: Channel::Dev,
            version: version.clone(),
            primary: PRIMARY_BINARY.to_string(),
        },
    )?;

    Ok(Installed {
        channel: Channel::Dev,
        version,
        root: destination,
        already_installed: replaced,
        // A build from the working tree has no publisher and therefore no
        // published digest; hashing bytes this process just produced would
        // verify nothing.
        verified: None,
    })
}

/// Walks up from `start` for the checkout markers.
///
/// The markers are the ones bundled discovery's checkout rule uses: a
/// `Cargo.toml` with `foundation/package.kira` beside it. Requiring both keeps
/// an arbitrary Rust workspace, or a stray directory named `foundation`, from
/// being built and installed as a compiler.
pub(crate) fn enclosing_checkout(start: &Path) -> Option<PathBuf> {
    start.ancestors().find_map(|candidate| {
        let manifest = candidate.join("Cargo.toml");
        let foundation = candidate
            .join("foundation")
            .join(PACKAGE_MANIFEST_FILE_NAME);
        (manifest.is_file() && foundation.is_file()).then(|| candidate.to_path_buf())
    })
}

/// The version the checkout's workspace manifest states.
fn workspace_version(checkout: &Path) -> Result<String, BinstallError> {
    let manifest = checkout.join("Cargo.toml");
    let unreadable = || BinstallError::VersionUnreadable {
        manifest: manifest.clone(),
    };
    let contents = std::fs::read_to_string(&manifest).map_err(|_| unreadable())?;
    let parsed: toml::Value = toml::from_str(&contents).map_err(|_| unreadable())?;
    parsed
        .get("workspace")
        .and_then(|workspace| workspace.get("package"))
        .and_then(|package| package.get("version"))
        .and_then(|version| version.as_str())
        .map(str::to_string)
        .ok_or_else(unreadable)
}

/// Where cargo put the build, honoring a `CARGO_TARGET_DIR` override.
pub(crate) fn target_dir(checkout: &Path) -> PathBuf {
    match std::env::var_os("CARGO_TARGET_DIR") {
        Some(overridden) if !overridden.is_empty() => PathBuf::from(overridden),
        _ => checkout.join("target"),
    }
}

/// Copies a directory tree; contents only, no metadata beyond what `copy` keeps.
fn copy_tree(source: &Path, destination: &Path) -> Result<(), InstallError> {
    create_dir(destination)?;
    let entries =
        std::fs::read_dir(source).map_err(|error| InstallError::io("read", source, error))?;
    for entry in entries {
        let entry = entry.map_err(|error| InstallError::io("read an entry of", source, error))?;
        let target = destination.join(entry.file_name());
        let kind = entry
            .file_type()
            .map_err(|error| InstallError::io("inspect", &entry.path(), error))?;
        if kind.is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), &target)
                .map_err(|error| InstallError::io("copy into", &target, error))?;
        }
    }
    Ok(())
}

/// Copies the separate debug-information file an executable has, if it has one.
///
/// Windows keeps it beside the image as `<stem>.pdb` and a profiler finds it by
/// that name, so an install that leaves it behind installs a binary nothing can
/// attribute a sample inside. Every other host this ships to keeps its debug
/// information *in* the binary, so there is nothing beside it to carry and the
/// absent file is the normal case rather than a failure.
fn copy_debug_info(built: &Path, staged: &Path) -> Result<(), InstallError> {
    let source = built.with_extension("pdb");
    if !source.is_file() {
        return Ok(());
    }
    let destination = staged.with_extension("pdb");
    std::fs::copy(&source, &destination)
        .map_err(|error| InstallError::io("copy the debug information to", &destination, error))?;
    Ok(())
}

/// `create_dir_all` with the crate's error shape.
fn create_dir(path: &Path) -> Result<(), InstallError> {
    std::fs::create_dir_all(path).map_err(|error| InstallError::io("create", path, error))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The compiler and the archive it links have to be built by one command,
    /// or a developer gets a toolchain whose two halves disagree about the
    /// runtime ABI. This has happened: `binstall` built only `kira-cli` and
    /// `kira-lsp`, staged the archive an older build had left behind, and
    /// every `kira run --backend llvm` afterwards failed on the marker.
    #[test]
    fn the_runtime_archives_crate_is_built_rather_than_assumed() {
        assert!(
            BUILD_PACKAGES.contains(&"kira-native-bridge"),
            "the staticlib is staged from the profile directory, so it has to be built there"
        );
        assert!(
            BUILD_PACKAGES.contains(&"kira-compiler-bridge"),
            "the compiler staticlib is staged from the profile directory, so it has to be built there"
        );
        assert!(BUILD_PACKAGES.contains(&"kira-cli"));
        assert!(BUILD_PACKAGES.contains(&"kira-lsp"));
    }

    /// Nothing in the workspace depends on the runner client, so a build that
    /// does not name it does not produce it — and `kira live` on the installed
    /// toolchain then builds a bundle and fails to start anything.
    #[test]
    fn the_desktop_runner_is_built_rather_than_inherited() {
        assert!(
            BUILD_PACKAGES.contains(&"kira-desktop-runner"),
            "`kira live` starts the runner from beside the compiler, so the \
             toolchain has to install one"
        );
    }

    /// The launcher's rlib *is* inherited, through `kira-cli`; its executable
    /// is not. A toolchain that stages only the rlib builds hybrid bundles and
    /// then has nothing to stage as their standalone executable.
    #[test]
    fn the_hybrid_launcher_is_built_rather_than_inherited() {
        assert!(
            BUILD_PACKAGES.contains(&"kira-hybrid-launcher"),
            "a hybrid build stages the launcher from beside the compiler, so \
             the toolchain has to install one"
        );
    }
}
