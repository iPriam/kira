//! Building this checkout into an installed dev toolchain.
//!
//! `knvm binstall` is the developer's install route: it compiles the compiler
//! out of the checkout it is run inside (dev profile), shapes the result into
//! the same tree a release archive unpacks to — `bin/kira` with `foundation/`
//! beside it — and lands it on the `dev` channel through the same
//! staging/validate/rename pipeline a release install uses. Installing selects
//! it, so `kira` dispatches to the fresh build immediately.
//!
//! Running it again replaces the installed tree. A dev toolchain names a
//! moving target, so "already installed" would mean "silently stale" — the
//! one thing a rebuild command must never be.

use std::path::{Path, PathBuf};
use std::process::Command;

use kira_toolchain::{Channel, CurrentToolchain, LANGUAGE_SERVER_BINARY, executable_name};

use crate::install::{
    InstallError, Installed, PRIMARY_BINARY, Staging, toolchain_root, validate, write_current,
};

/// The manifest that marks the bundled Foundation as a real Kira package.
const PACKAGE_MANIFEST_FILE_NAME: &str = "package.kira";

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
    /// Staging, validating, or landing the tree failed.
    #[error(transparent)]
    Install(#[from] InstallError),
}

/// The packages `binstall` builds before it stages a toolchain.
///
/// Both runtime crates are named even though `kira-cli` depends on them,
/// because those dependencies build their **rlibs** and a toolchain installs
/// their **staticlibs** — cargo builds a crate-type nobody asked for only when
/// the crate itself is a target. Leaving either out installs a fresh compiler
/// beside an absent or stale archive: ordinary native programs need the base
/// bridge, while programs that call the compiler need its superset.
const BUILD_PACKAGES: [&str; 4] = [
    "kira-cli",
    "kira-lsp",
    "kira-native-bridge",
    "kira-compiler-bridge",
];

/// Builds the enclosing checkout and installs it as the selected dev toolchain.
///
/// `start` is where the checkout search begins — the working directory, for the
/// binary. The build's stdout and stderr are inherited, so a compile error
/// lands in front of the user rather than in a captured buffer.
pub fn binstall(toolchains_root: &Path, start: &Path) -> Result<Installed, BinstallError> {
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
    let cross = Command::new("cargo")
        .args([
            "build",
            "-p",
            "kira-native-bridge",
            "--target",
            "wasm32-unknown-emscripten",
        ])
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

    let debug_dir = target_dir(&checkout).join("debug");
    let compiler = debug_dir.join(executable_name(PRIMARY_BINARY));
    let language_server = debug_dir.join(executable_name(LANGUAGE_SERVER_BINARY));
    let host_archive = debug_dir.join("libkira_native_bridge.a");
    let compiler_archive = debug_dir.join("libkira_compiler_bridge.a");
    let wasm_archive = target_dir(&checkout)
        .join("wasm32-unknown-emscripten")
        .join("debug")
        .join("libkira_native_bridge.a");
    for artifact in [
        &compiler,
        &language_server,
        &host_archive,
        &compiler_archive,
        &wasm_archive,
    ] {
        if !artifact.is_file() {
            return Err(BinstallError::MissingBuildArtifact {
                expected: artifact.clone(),
            });
        }
    }

    // The same discipline as a release install: shape and validate the whole
    // tree in staging, then swap it into place. The staging guard also carries
    // the replaced tree out, so a failure after the swap point cannot lose it.
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
    // The runtime archives ride beside the compiler, where its archive
    // resolution looks: the host's under its cargo name, the Web's under a
    // target-suffixed one so neither can be linked in the other's place.
    let staged_host = bin.join("libkira_native_bridge.a");
    std::fs::copy(&host_archive, &staged_host)
        .map_err(|error| InstallError::io("copy the runtime archive to", &staged_host, error))?;
    let staged_compiler = bin.join("libkira_compiler_bridge.a");
    std::fs::copy(&compiler_archive, &staged_compiler).map_err(|error| {
        InstallError::io(
            "copy the compiler runtime archive to",
            &staged_compiler,
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
    if replaced {
        let retired = staging.path().join("replaced");
        std::fs::rename(&destination, &retired).map_err(|error| {
            InstallError::io("set aside the previous build at", &destination, error)
        })?;
    }
    std::fs::rename(&payload, &destination)
        .map_err(|error| InstallError::io("move the built toolchain into", &destination, error))?;

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
            "the staticlib is staged from `target/debug`, so it has to be built there"
        );
        assert!(
            BUILD_PACKAGES.contains(&"kira-compiler-bridge"),
            "the compiler staticlib is staged from `target/debug`, so it has to be built there"
        );
        assert!(BUILD_PACKAGES.contains(&"kira-cli"));
        assert!(BUILD_PACKAGES.contains(&"kira-lsp"));
    }
}
