//! Linking and archiving the artifacts that are not executables: the shared
//! library a Kira library produces, the whole-program library a live runner
//! loads, the native half of a hybrid program, and the self-contained static
//! archive a Rust consumer links.

use std::path::{Path, PathBuf};
use std::process::Command;

use kira_native_lib_definition::NativeLinkInputs;
use kira_toolchain::LlvmInstallation;

use super::driver::{
    export_debug_symbols, force_callback_symbols, force_host_symbols,
    native_live_runtime_arguments, shared_library_flag, tool_diagnostic,
};
use super::target::NativeBuildTarget;
use super::{LinkError, link_with};

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
    target: &NativeBuildTarget,
) -> Result<(), LinkError> {
    let mut arguments = vec![shared_library_flag(target).to_owned()];
    arguments.extend(force_host_symbols(target));
    link_with(
        llvm,
        &[object.to_path_buf()],
        runtime_archive,
        &NativeLinkInputs::default(),
        library,
        &arguments,
        target,
    )
}

/// Links a whole native program into the shared library an LLVM live runner
/// loads and invokes.
///
/// The native library carries the complete program, the Kira runtime, and the
/// selected foreign link inputs. Its run-path points at its own directory on
/// POSIX so bundled dynamic dependencies resolve after the runner stages the
/// library and assets together.
///
/// Always the host's: the runner loads this library into its own process, so a
/// live session for another machine is not a thing that could run.
pub fn link_native_live_library(
    llvm: &LlvmInstallation,
    objects: &[PathBuf],
    runtime_archive: &Path,
    foreign_link: &NativeLinkInputs,
    library: &Path,
) -> Result<(), LinkError> {
    let target = NativeBuildTarget::host();
    let mut arguments = vec![shared_library_flag(&target).to_owned()];
    arguments.extend(force_host_symbols(&target));
    arguments.extend(native_live_runtime_arguments());
    link_with(
        llvm,
        objects,
        runtime_archive,
        foreign_link,
        library,
        &arguments,
        &target,
    )
}

/// Links the native half of a hybrid program: the hybrid object plus the
/// selected C archives and the runtime, into one shared library.
///
/// The hybrid half carries the `@Native` trampolines, callback thunks, and any
/// opted-in static FFI symbols shared with its runtime half. It is loaded by the
/// interpreter running in this process, so it is the host's like the live
/// library above.
pub fn link_hybrid_library(
    llvm: &LlvmInstallation,
    object: &Path,
    runtime_archive: &Path,
    foreign_link: &NativeLinkInputs,
    retained_symbols: &[String],
    library: &Path,
) -> Result<(), LinkError> {
    link_hybrid_library_inner(
        llvm,
        object,
        runtime_archive,
        foreign_link,
        library,
        HybridLinkOptions {
            retained_symbols,
            debug_symbols: None,
        },
    )
}

/// Links a hybrid native half while retaining its debugger symbols.
pub fn link_hybrid_library_debug(
    llvm: &LlvmInstallation,
    object: &Path,
    runtime_archive: &Path,
    foreign_link: &NativeLinkInputs,
    retained_symbols: &[String],
    library: &Path,
    debug_symbols: &[String],
) -> Result<(), LinkError> {
    link_hybrid_library_inner(
        llvm,
        object,
        runtime_archive,
        foreign_link,
        library,
        HybridLinkOptions {
            retained_symbols,
            debug_symbols: Some(debug_symbols),
        },
    )
}

#[derive(Debug, Clone, Copy)]
struct HybridLinkOptions<'a> {
    retained_symbols: &'a [String],
    debug_symbols: Option<&'a [String]>,
}

fn link_hybrid_library_inner(
    llvm: &LlvmInstallation,
    object: &Path,
    runtime_archive: &Path,
    foreign_link: &NativeLinkInputs,
    library: &Path,
    options: HybridLinkOptions<'_>,
) -> Result<(), LinkError> {
    let target = NativeBuildTarget::host();
    let mut arguments = vec![shared_library_flag(&target).to_owned()];
    arguments.extend(force_host_symbols(&target));
    if !options.retained_symbols.is_empty() {
        arguments.extend(force_callback_symbols(&target, options.retained_symbols));
    }
    if let Some(debug_symbols) = options.debug_symbols {
        arguments.push("-g".to_owned());
        arguments.extend(export_debug_symbols(&target, debug_symbols));
    }
    link_with(
        llvm,
        &[object.to_path_buf()],
        runtime_archive,
        foreign_link,
        library,
        &arguments,
        &target,
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
/// No target is threaded through here and none is needed: an archive is a
/// container, and `llvm-ar` copies members without reading the machine code in
/// them. What decides the architecture is which objects were put in, which the
/// emission upstream already settled.
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
            diagnostic: tool_diagnostic(&output),
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
