//! Linking a native executable.
//!
//! Codegen happens in process through LLVM; `clang` is used only here, as the
//! linker driver, and always the `clang` from the discovered LLVM install
//! rather than whatever is on `PATH` — the same explicit-toolchain rule the
//! rest of the backend follows.
//!
//! The link inputs are the program's object file and the native runtime archive
//! (`libkira_native_bridge.a`), which is a Rust `staticlib` and therefore
//! carries the Rust standard library with it; the driver supplies the system
//! libraries around it.

use std::path::{Path, PathBuf};
use std::process::Command;

use kira_toolchain::LlvmInstallation;

/// Why linking failed.
#[derive(Debug, thiserror::Error)]
pub enum LinkError {
    /// The discovered LLVM install has no `clang` driver.
    #[error("no `clang` linker driver at `{path}` in the discovered LLVM install")]
    DriverMissing {
        /// Where `clang` was expected.
        path: PathBuf,
    },
    /// The native runtime archive is missing.
    #[error(
        "the native runtime archive `{path}` is missing; build it with \
         `cargo build -p kira-native-bridge`"
    )]
    RuntimeArchiveMissing {
        /// Where the archive was expected.
        path: PathBuf,
    },
    /// The linker driver could not be run at all.
    #[error("cannot run the linker driver `{driver}`: {source}")]
    DriverUnusable {
        /// The driver that could not be spawned.
        driver: PathBuf,
        /// The underlying failure.
        #[source]
        source: std::io::Error,
    },
    /// The runtime archive was built against a different `kira_rt_*` contract.
    ///
    /// Caught by name at link time rather than by corruption at run time: this
    /// is exactly the failure the ABI marker exists to make loud.
    #[error(
        "the native runtime archive `{path}` was built against a different \
         version of the runtime ABI (it does not define `{marker}`); rebuild it \
         with `cargo build -p kira-native-bridge`"
    )]
    RuntimeArchiveStale {
        /// The stale archive.
        path: PathBuf,
        /// The marker this compiler expected it to define.
        marker: &'static str,
    },
    /// The linker ran and rejected the link.
    #[error("linking `{output}` failed:\n{stderr}")]
    Failed {
        /// The executable being linked.
        output: PathBuf,
        /// The driver's diagnostics.
        stderr: String,
    },
}

/// Links `object` against the native runtime archive into a shared library.
///
/// The library is self-contained: it carries the runtime archive, so it has no
/// undefined symbols and needs no arrangement with whatever process loads it.
/// The host reaches in through trampolines and hands its invoker back in.
///
/// The host also resolves a handful of runtime symbols by name, and those need
/// forcing in — see [`force_host_symbols`].
pub fn link_shared_library(
    llvm: &LlvmInstallation,
    object: &Path,
    runtime_archive: &Path,
    library: &Path,
) -> Result<(), LinkError> {
    // Apple's linker spells "shared library" differently from everyone else's.
    let shared_flag = if cfg!(target_os = "macos") {
        "-dynamiclib"
    } else {
        "-shared"
    };
    let mut arguments = vec![shared_flag.to_owned()];
    arguments.extend(force_host_symbols());
    link(llvm, object, runtime_archive, library, &arguments)
}

/// Links `object` against the native runtime archive into `executable`.
pub fn link_executable(
    llvm: &LlvmInstallation,
    object: &Path,
    runtime_archive: &Path,
    executable: &Path,
) -> Result<(), LinkError> {
    link(llvm, object, runtime_archive, executable, &[])
}

/// Linker flags that pull the host-facing runtime symbols into a shared library.
///
/// A linker pulls only *referenced* members out of an archive, and nothing in
/// generated code references these: the host calls
/// `kira_hybrid_install_runtime_invoker` itself, and the string helpers are
/// reached only by a program that happens to use strings. Without this, a
/// hybrid library built from a program with no strings in its native half
/// carries no `kira_rt_str_new` — and the host's `dlsym` fails on a library
/// that is otherwise perfectly good.
///
/// Each symbol is requested as an undefined one, which makes the linker pull in
/// exactly the member defining it. That is deliberately narrower than
/// force-loading the whole archive (`-force_load` / `--whole-archive`), which
/// would drag every unreferenced member of a Rust `staticlib` — the entire
/// standard library — into every hybrid program.
///
/// `kira_runtime_abi::HYBRID_HOST_SYMBOLS` is the same list the host resolves
/// from, so what is forced in and what is looked up cannot drift.
fn force_host_symbols() -> Vec<String> {
    kira_runtime_abi::HYBRID_HOST_SYMBOLS
        .iter()
        .map(|symbol| {
            if cfg!(target_os = "macos") {
                // Mach-O prefixes C symbols with an underscore; ELF does not.
                format!("-Wl,-u,_{symbol}")
            } else {
                format!("-Wl,--undefined={symbol}")
            }
        })
        .collect()
}

/// Runs the linker driver over `object` plus the runtime archive.
fn link(
    llvm: &LlvmInstallation,
    object: &Path,
    runtime_archive: &Path,
    output: &Path,
    extra: &[String],
) -> Result<(), LinkError> {
    let executable = output;
    let driver = llvm.clang();
    if !driver.is_file() {
        return Err(LinkError::DriverMissing { path: driver });
    }
    if !runtime_archive.is_file() {
        return Err(LinkError::RuntimeArchiveMissing {
            path: runtime_archive.to_path_buf(),
        });
    }

    let mut command = Command::new(&driver);
    command
        .arg(object)
        .arg(runtime_archive)
        .arg("-o")
        .arg(executable);
    for argument in extra {
        command.arg(argument);
    }
    for argument in platform_link_arguments() {
        command.arg(argument);
    }
    // The managed clang is not Apple's, so it has no built-in knowledge of
    // where the platform libraries live; without an explicit sysroot the link
    // fails on `library 'System' not found`.
    if let Some(sysroot) = macos_sysroot() {
        command.arg("-isysroot").arg(sysroot);
    }

    let output = command
        .output()
        .map_err(|source| LinkError::DriverUnusable {
            driver: driver.clone(),
            source,
        })?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        // The ABI marker is the one undefined symbol with a known cause, so say
        // the cause rather than making the reader decode a linker diagnostic.
        if stderr.contains(kira_runtime_abi::RUNTIME_ABI_MARKER) {
            return Err(LinkError::RuntimeArchiveStale {
                path: runtime_archive.to_path_buf(),
                marker: kira_runtime_abi::RUNTIME_ABI_MARKER,
            });
        }
        return Err(LinkError::Failed {
            output: executable.to_path_buf(),
            stderr,
        });
    }
    Ok(())
}

/// The system libraries the Rust `staticlib` runtime needs on this host.
///
/// A Rust static archive bundles the standard library but not the platform
/// libraries it calls into; these are the libraries `rustc --print
/// native-static-libs` reports for each host, minus the ones the clang driver
/// already links by default (`-lSystem`, `-lc`).
fn platform_link_arguments() -> Vec<String> {
    if cfg!(target_os = "macos") {
        // Rust's std on Apple platforms resolves names and unwinds through
        // these; the driver supplies libSystem itself.
        ["-lresolv", "-lc++", "-framework", "CoreFoundation"]
            .into_iter()
            .map(str::to_owned)
            .collect()
    } else if cfg!(target_os = "linux") {
        ["-lpthread", "-ldl", "-lm", "-lrt", "-lgcc_s", "-lutil"]
            .into_iter()
            .map(str::to_owned)
            .collect()
    } else {
        Vec::new()
    }
}

/// The macOS SDK to link against, or `None` off macOS.
///
/// Asks `xcrun`, the same way every other macOS toolchain finds the SDK, and
/// honours an explicit `SDKROOT` first. Returning `None` when `xcrun` cannot
/// answer leaves the driver to its own defaults rather than passing a path that
/// does not exist.
fn macos_sysroot() -> Option<PathBuf> {
    if !cfg!(target_os = "macos") {
        return None;
    }
    if let Some(root) = std::env::var_os("SDKROOT")
        && !root.is_empty()
    {
        return Some(PathBuf::from(root));
    }
    let output = Command::new("xcrun").arg("--show-sdk-path").output().ok()?;
    if !output.status.success() {
        return None;
    }
    let path = String::from_utf8(output.stdout).ok()?;
    let trimmed = path.trim();
    (!trimmed.is_empty()).then(|| PathBuf::from(trimmed))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_runtime_archive_names_how_to_build_it() {
        let error = LinkError::RuntimeArchiveMissing {
            path: PathBuf::from("/nowhere/libkira_native_bridge.a"),
        };
        let text = error.to_string();
        assert!(text.contains("libkira_native_bridge.a"));
        assert!(text.contains("cargo build -p kira-native-bridge"));
    }

    #[test]
    fn every_host_link_argument_is_a_library_or_framework_flag() {
        // Guards against a stray path or empty string sneaking into the link
        // line, which the driver would silently treat as an input file.
        for argument in platform_link_arguments() {
            assert!(
                argument.starts_with('-') || argument == "CoreFoundation",
                "unexpected link argument `{argument}`",
            );
        }
    }
}
