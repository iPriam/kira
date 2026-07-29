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

use kira_native_lib_definition::NativeLinkInputs;
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
    /// The discovered LLVM install has no `llvm-ar` archiver.
    #[error("no `llvm-ar` archiver at `{path}` in the discovered LLVM install")]
    ArchiverMissing {
        /// Where `llvm-ar` was expected.
        path: PathBuf,
    },
    /// The archive's directory could not be prepared, or a stale archive could
    /// not be removed.
    #[error("cannot write the static archive `{path}`: {source}")]
    ArchiveUnwritable {
        /// The archive that could not be written.
        path: PathBuf,
        /// The underlying I/O failure.
        #[source]
        source: std::io::Error,
    },
    /// The archiver reported success and wrote nothing.
    ///
    /// Worth its own name: a consumer that linked against a path with no file
    /// there gets a diagnostic about a missing library rather than about the
    /// build step that was supposed to produce it.
    #[error("the archiver reported success but wrote no archive at `{path}`")]
    ArchiveMissing {
        /// Where the archive was expected.
        path: PathBuf,
    },
    /// The native runtime archive is missing.
    #[error(
        "the native runtime archive `{path}` is missing; build it with \
         `cargo build --workspace`"
    )]
    RuntimeArchiveMissing {
        /// Where the archive was expected.
        path: PathBuf,
    },
    /// A selected foreign C static archive is missing.
    ///
    /// Named on its own so a package pointing `nativeLibraries` at an archive
    /// that is not there gets a diagnostic about the missing library file rather
    /// than an opaque linker error about undefined foreign symbols.
    #[error("the foreign native archive `{path}` is missing")]
    ForeignArchiveMissing {
        /// Where the archive was expected.
        path: PathBuf,
    },
    /// The compiled foreign shim object was not where the build left it.
    ///
    /// Named on its own because every adapter in a by-value program calls into
    /// this object: without it the link fails on a wall of undefined
    /// `kira_ffi_shim_*` symbols that says nothing about the missing file.
    #[error("the generated foreign shim object `{path}` is missing")]
    ShimObjectMissing {
        /// Where the object was expected.
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
    link_with(
        llvm,
        object,
        runtime_archive,
        &NativeLinkInputs::default(),
        None,
        library,
        &arguments,
    )
}

/// Combines `object` and the native runtime archive into one static archive.
///
/// This is what a Rust consumer of a Kira library links: one file carrying the
/// library's own code and every runtime member it needs. A consumer linking two
/// files — the library and the toolchain's runtime archive — would have to know
/// where the second one lives, which is exactly the arrangement with the Kira
/// toolchain a generated crate must not require.
///
/// # Why an MRI script
///
/// `ar` cannot merge an archive into another by naming it: `ar rcs out.a in.a`
/// adds `in.a` as a *member*, producing an archive containing an archive, which
/// no linker unpacks. `llvm-ar`'s MRI mode is the one portable way to say "take
/// that archive's members": `ADDLIB` splices them in, `ADDMOD` adds the object,
/// and `SAVE` writes the result — no extract-to-a-temporary-directory dance, and
/// no member name colliding with another on the way through.
///
/// Existing output is removed first. `CREATE` truncates, but only after the tool
/// has decided it can write there, and a stale archive left behind by a failed
/// run is worse than no archive: it links, and it is wrong.
pub fn archive_static_library(
    llvm: &LlvmInstallation,
    object: &Path,
    runtime_archive: &Path,
    archive: &Path,
) -> Result<(), LinkError> {
    let archiver = llvm.llvm_ar();
    if !archiver.is_file() {
        return Err(LinkError::ArchiverMissing { path: archiver });
    }
    if !runtime_archive.is_file() {
        return Err(LinkError::RuntimeArchiveMissing {
            path: runtime_archive.to_path_buf(),
        });
    }
    if let Some(parent) = archive.parent() {
        std::fs::create_dir_all(parent).map_err(|source| LinkError::ArchiveUnwritable {
            path: archive.to_path_buf(),
            source,
        })?;
    }
    if archive.exists() {
        std::fs::remove_file(archive).map_err(|source| LinkError::ArchiveUnwritable {
            path: archive.to_path_buf(),
            source,
        })?;
    }

    let script = mri_script(object, runtime_archive, archive);
    let mut command = Command::new(&archiver);
    command
        .arg("-M")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let mut child = command
        .spawn()
        .map_err(|source| LinkError::DriverUnusable {
            driver: archiver.clone(),
            source,
        })?;
    if let Some(stdin) = child.stdin.as_mut() {
        use std::io::Write;
        stdin
            .write_all(script.as_bytes())
            .map_err(|source| LinkError::DriverUnusable {
                driver: archiver.clone(),
                source,
            })?;
    }
    let output = child
        .wait_with_output()
        .map_err(|source| LinkError::DriverUnusable {
            driver: archiver.clone(),
            source,
        })?;
    if !output.status.success() {
        return Err(LinkError::Failed {
            output: archive.to_path_buf(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        });
    }
    if !archive.is_file() {
        return Err(LinkError::ArchiveMissing {
            path: archive.to_path_buf(),
        });
    }
    Ok(())
}

/// The MRI script that merges the runtime archive and the library's object.
///
/// Built as its own function so the exact commands are assertable without
/// running an archiver, which is the only part of this that a machine with no
/// LLVM can check.
fn mri_script(object: &Path, runtime_archive: &Path, archive: &Path) -> String {
    format!(
        "CREATE {}\nADDLIB {}\nADDMOD {}\nSAVE\nEND\n",
        archive.display(),
        runtime_archive.display(),
        object.display(),
    )
}

/// Links `object` against the native runtime archive into `executable`.
///
/// `foreign_link` are the resolved C link inputs that satisfy the program's
/// `@FFI.Extern` imports. Each generated adapter references its C symbol, so
/// naming the archives on the link line is enough for the linker to pull in
/// exactly the members those symbols need — no force-loading, and a missing
/// symbol becomes a named link error rather than a silent empty binary. The
/// frameworks, system libraries, and linker flags declared beside those
/// archives follow them on the line, because a library may supply its symbols
/// through those alone.
///
/// `shim` is the compiled C shim object, present only for a program that passes
/// a struct by value. Each adapter calls `kira_ffi_shim_<i>` instead of the real
/// symbol in that case, so without it the link fails on an undefined shim.
pub fn link_executable(
    llvm: &LlvmInstallation,
    object: &Path,
    runtime_archive: &Path,
    foreign_link: &NativeLinkInputs,
    shim: Option<&Path>,
    executable: &Path,
) -> Result<(), LinkError> {
    link_with(
        llvm,
        object,
        runtime_archive,
        foreign_link,
        shim,
        executable,
        &[],
    )
}

/// Links the VM's foreign-adapter sidecar: the adapters-only object plus the
/// selected C archives and the native runtime, into one shared library the host
/// loads through `kira-dynamic-ffi`.
///
/// Every adapter, the foreign-adapter marker, and the string helpers the loader
/// resolves are forced in by name: nothing inside the sidecar references them
/// (the sidecar has no call sites), so without forcing the linker would drop
/// them and the host's `dlsym` would fail on a library that is otherwise sound.
pub fn link_adapter_sidecar(
    llvm: &LlvmInstallation,
    object: &Path,
    runtime_archive: &Path,
    foreign_link: &NativeLinkInputs,
    shim: Option<&Path>,
    adapter_symbols: &[String],
    library: &Path,
) -> Result<(), LinkError> {
    let shared_flag = if cfg!(target_os = "macos") {
        "-dynamiclib"
    } else {
        "-shared"
    };
    let mut arguments = vec![shared_flag.to_owned()];
    arguments.extend(force_foreign_symbols(adapter_symbols));
    link_with(
        llvm,
        object,
        runtime_archive,
        foreign_link,
        shim,
        library,
        &arguments,
    )
}

/// Links the native half of a hybrid program: the hybrid object plus the
/// selected C archives and the runtime, into one shared library.
///
/// The hybrid half carries both the `@Native` trampolines and one generated
/// adapter per foreign import. Both symbol families need forcing in: the host
/// resolves the trampoline-adjacent runtime symbols (see [`force_host_symbols`])
/// and — when the program has foreign imports — the foreign-adapter marker, the
/// string helpers, and every adapter by name (see [`force_foreign_symbols`]), so
/// the hybrid session binds every foreign call out of this one dylib rather than
/// opening a second sidecar. The C archives satisfy each adapter's reference to
/// its real C symbol.
pub fn link_hybrid_library(
    llvm: &LlvmInstallation,
    object: &Path,
    runtime_archive: &Path,
    foreign_link: &NativeLinkInputs,
    shim: Option<&Path>,
    adapter_symbols: &[String],
    library: &Path,
) -> Result<(), LinkError> {
    let shared_flag = if cfg!(target_os = "macos") {
        "-dynamiclib"
    } else {
        "-shared"
    };
    let mut arguments = vec![shared_flag.to_owned()];
    arguments.extend(force_host_symbols());
    // A program with no foreign imports emits no adapters and no foreign marker,
    // so forcing either would fail the link on an undefined symbol.
    if !adapter_symbols.is_empty() {
        arguments.extend(force_foreign_symbols(adapter_symbols));
    }
    link_with(
        llvm,
        object,
        runtime_archive,
        foreign_link,
        shim,
        library,
        &arguments,
    )
}

/// The `-Wl,-u` flags that force every foreign-sidecar symbol a host resolves.
///
/// The foreign-adapter marker and the string helpers the loader binds, plus one
/// entry per generated adapter, so a host loading the sidecar finds each by
/// `dlsym`. The marker and helper names come from the shared runtime-abi
/// constants, so what is forced in and what the loader demands cannot drift.
fn force_foreign_symbols(adapter_symbols: &[String]) -> Vec<String> {
    let mut names: Vec<String> = vec![kira_runtime_abi::FOREIGN_ADAPTER_ABI_MARKER.to_owned()];
    names.extend(
        [
            kira_runtime_abi::FOREIGN_STRING_NEW_SYMBOL,
            kira_runtime_abi::FOREIGN_STRING_FREE_SYMBOL,
            kira_runtime_abi::FOREIGN_STRING_DATA_SYMBOL,
            kira_runtime_abi::FOREIGN_STRING_LEN_SYMBOL,
        ]
        .into_iter()
        .map(str::to_owned),
    );
    names.extend(adapter_symbols.iter().cloned());
    names
        .into_iter()
        .map(|symbol| {
            if cfg!(target_os = "macos") {
                format!("-Wl,-u,_{symbol}")
            } else {
                format!("-Wl,--undefined={symbol}")
            }
        })
        .collect()
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

/// Runs the linker driver over `object` plus the runtime archive and any
/// selected foreign C archives.
fn link_with(
    llvm: &LlvmInstallation,
    object: &Path,
    runtime_archive: &Path,
    foreign_link: &NativeLinkInputs,
    shim: Option<&Path>,
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
    for archive in foreign_link.archives() {
        if !archive.is_file() {
            return Err(LinkError::ForeignArchiveMissing {
                path: archive.clone(),
            });
        }
    }

    if let Some(shim) = shim
        && !shim.is_file()
    {
        return Err(LinkError::ShimObjectMissing {
            path: shim.to_path_buf(),
        });
    }

    let mut command = Command::new(&driver);
    command.arg(object);
    // The shim object sits between the program and the archives: the adapters in
    // `object` call into it, and it calls the real C symbols the archives define.
    if let Some(shim) = shim {
        command.arg(shim);
    }
    // The foreign archives precede the runtime archive so an adapter's reference
    // to a C symbol is satisfied by the archive that defines it.
    for archive in foreign_link.archives() {
        command.arg(archive);
    }
    command.arg(runtime_archive).arg("-o").arg(executable);
    // The frameworks, system libraries, and linker flags the selected rows
    // declared. They follow the archives for the same reason the archives
    // precede the runtime: a system library resolves symbols left of it.
    for argument in foreign_link.driver_arguments() {
        command.arg(argument);
    }
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

/// The system libraries the Rust `staticlib` runtime needs on this host, spelled
/// as driver arguments.
///
/// The names come from [`crate::platform`], which is the one place they are
/// written down — the generated `build.rs` a consumer links through reads the
/// same list, so a library added for one is added for both.
fn platform_link_arguments() -> Vec<String> {
    let list = crate::platform::host_link_list();
    let mut arguments: Vec<String> = list
        .libraries
        .iter()
        .map(|library| format!("-l{library}"))
        .collect();
    for framework in list.frameworks {
        arguments.push("-framework".to_owned());
        arguments.push((*framework).to_owned());
    }
    arguments
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
    fn the_sidecar_forces_the_marker_string_helpers_and_every_adapter() {
        // The host resolves each of these by name, and nothing inside the
        // sidecar references them, so the link must force every one in.
        let forced = force_foreign_symbols(&["kira_foreign_adapter_0".to_owned()]);
        let joined = forced.join(" ");
        for symbol in [
            kira_runtime_abi::FOREIGN_ADAPTER_ABI_MARKER,
            kira_runtime_abi::FOREIGN_STRING_NEW_SYMBOL,
            kira_runtime_abi::FOREIGN_STRING_FREE_SYMBOL,
            kira_runtime_abi::FOREIGN_STRING_DATA_SYMBOL,
            kira_runtime_abi::FOREIGN_STRING_LEN_SYMBOL,
            "kira_foreign_adapter_0",
        ] {
            assert!(joined.contains(symbol), "sidecar must force `{symbol}`");
        }
        // Every entry is an undefined-symbol linker flag, never a bare input.
        for argument in &forced {
            assert!(argument.starts_with("-Wl,"), "unexpected flag `{argument}`");
        }
    }

    #[test]
    fn a_missing_foreign_archive_names_the_missing_file() {
        let error = LinkError::ForeignArchiveMissing {
            path: PathBuf::from("/nowhere/libffimath.a"),
        };
        assert!(error.to_string().contains("libffimath.a"));
    }

    #[test]
    fn a_missing_runtime_archive_names_how_to_build_it() {
        let error = LinkError::RuntimeArchiveMissing {
            path: PathBuf::from("/nowhere/libkira_native_bridge.a"),
        };
        let text = error.to_string();
        assert!(text.contains("libkira_native_bridge.a"));
        assert!(text.contains("cargo build --workspace"));
    }

    #[test]
    fn the_archive_script_splices_the_runtime_in_rather_than_nesting_it() {
        // `ADDLIB` takes the runtime archive's *members*; `ADDMOD` would nest
        // the archive inside the result, which links as nothing.
        let script = mri_script(
            Path::new("/build/uifoundation.o"),
            Path::new("/build/libkira_native_bridge.a"),
            Path::new("/build/lib/libuifoundation.a"),
        );
        assert_eq!(
            script,
            "CREATE /build/lib/libuifoundation.a\n\
             ADDLIB /build/libkira_native_bridge.a\n\
             ADDMOD /build/uifoundation.o\n\
             SAVE\n\
             END\n"
        );
    }

    #[test]
    fn a_missing_archiver_names_the_tool_and_where_it_was_looked_for() {
        let error = LinkError::ArchiverMissing {
            path: PathBuf::from("/llvm/bin/llvm-ar"),
        };
        let text = error.to_string();
        assert!(text.contains("llvm-ar"), "{text}");
        assert!(text.contains("/llvm/bin/llvm-ar"), "{text}");
    }

    #[test]
    fn every_host_link_argument_is_a_library_or_framework_flag() {
        // Guards against a stray path or empty string sneaking into the link
        // line, which the driver would silently treat as an input file.
        let frameworks = crate::platform::host_link_list().frameworks;
        for argument in platform_link_arguments() {
            assert!(
                argument.starts_with('-') || frameworks.contains(&argument.as_str()),
                "unexpected link argument `{argument}`",
            );
        }
    }
}
