//! Linking and archiving the artifacts that are not executables: the shared
//! library a Kira library produces, the whole-program library a live runner
//! loads, the native half of a hybrid program, and the self-contained static
//! archive a Rust consumer links.

use std::path::{Path, PathBuf};
use std::process::Command;

use kira_native_lib_definition::NativeLinkInputs;
use kira_toolchain::LlvmInstallation;

use super::driver::{
    bind_own_symbols_locally, export_debug_symbols, force_callback_symbols, force_host_symbols,
    native_live_runtime_arguments, shared_library_flag, tool_diagnostic,
};
use super::target::NativeBuildTarget;
use super::{LinkError, LinkOptions, link_with};

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
    options: LinkOptions<'_>,
) -> Result<(), LinkError> {
    let mut arguments = vec![shared_library_flag(options.target).to_owned()];
    arguments.extend(force_host_symbols(options.target));
    link_with(
        llvm,
        &[object.to_path_buf()],
        runtime_archive,
        &NativeLinkInputs::default(),
        library,
        &arguments,
        options,
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
    sanitize: crate::Sanitize,
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
        LinkOptions {
            target: &target,
            sanitize,
            shared: true,
        },
    )
}

/// Links the native half of a hybrid program: the hybrid object plus the
/// selected C archives and the runtime, into one shared library.
///
/// The hybrid half carries the `@Native` trampolines, callback thunks, and any
/// opted-in static FFI symbols shared with its runtime half. It is loaded by the
/// interpreter running in this process, so it is the host's like the live
/// library above.
pub(crate) fn link_hybrid_library(
    llvm: &LlvmInstallation,
    object: &Path,
    runtime_archive: &Path,
    foreign_link: &NativeLinkInputs,
    library: &Path,
    options: HybridLinkOptions<'_>,
) -> Result<(), LinkError> {
    link_hybrid_library_inner(
        llvm,
        object,
        runtime_archive,
        foreign_link,
        library,
        options,
    )
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct HybridLinkOptions<'a> {
    pub(crate) retained_symbols: &'a [String],
    pub(crate) debug_symbols: Option<&'a [String]>,
    pub(crate) sanitize: crate::Sanitize,
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
    arguments.extend(bind_own_symbols_locally(&target));
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
        LinkOptions {
            target: &target,
            sanitize: options.sanitize,
            shared: true,
        },
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

    // The script names its inputs bare, so the tool runs from a staging
    // directory where each input sits under a fixed space-free name and the
    // archive is written beside them, then moved into place.
    let staging = match staging_dir(archive) {
        Ok(staging) => staging,
        Err(source) => {
            return Err(LinkError::ArchiveUnwritable {
                path: archive.to_path_buf(),
                source,
            });
        }
    };
    let staged_object = staging.join("object.o");
    let staged_runtime = staging.join("runtime.a");
    let staged_archive = staging.join("out.a");
    if let Err(source) = stage_input(object, &staged_object)
        .and_then(|()| stage_input(runtime_archive, &staged_runtime))
    {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(LinkError::ArchiveUnwritable {
            path: archive.to_path_buf(),
            source,
        });
    }

    let script = mri_script(
        staged_object
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .as_ref(),
        staged_runtime
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .as_ref(),
        staged_archive
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .as_ref(),
    );
    let mut command = Command::new(&archiver);
    command
        .arg("-M")
        .current_dir(&staging)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(source) => {
            let _ = std::fs::remove_dir_all(&staging);
            return Err(LinkError::DriverUnusable {
                driver: archiver.clone(),
                source,
            });
        }
    };
    if let Some(stdin) = child.stdin.as_mut() {
        use std::io::Write;
        if let Err(source) = stdin.write_all(script.as_bytes()) {
            let _ = child.kill();
            let _ = std::fs::remove_dir_all(&staging);
            return Err(LinkError::DriverUnusable {
                driver: archiver.clone(),
                source,
            });
        }
    }
    let output = match child.wait_with_output() {
        Ok(output) => output,
        Err(source) => {
            let _ = std::fs::remove_dir_all(&staging);
            return Err(LinkError::DriverUnusable {
                driver: archiver.clone(),
                source,
            });
        }
    };
    let result = if !output.status.success() {
        Err(LinkError::Failed {
            output: archive.to_path_buf(),
            diagnostic: tool_diagnostic(&output),
        })
    } else {
        std::fs::copy(&staged_archive, archive)
            .map(|_| ())
            .map_err(|source| LinkError::ArchiveUnwritable {
                path: archive.to_path_buf(),
                source,
            })
    };
    let _ = std::fs::remove_dir_all(&staging);
    result?;
    if !archive.is_file() {
        return Err(LinkError::ArchiveMissing {
            path: archive.to_path_buf(),
        });
    }
    Ok(())
}

/// A fresh directory beside `archive` to stage space-free input names in.
fn staging_dir(archive: &Path) -> std::io::Result<PathBuf> {
    let parent = archive.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent)?;
    let base = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| since.as_nanos())
        .unwrap_or(0);
    for attempt in 0..64u128 {
        let candidate = parent.join(format!(".kira-archive-{base}-{attempt}"));
        match std::fs::create_dir(&candidate) {
            Ok(()) => return Ok(candidate),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "no free staging directory name",
    ))
}

/// Puts `input` at `staged` under its space-free name.
///
/// A link is preferred where the OS supports it — the archiver only reads —
/// and a copy everywhere else.
#[cfg(unix)]
fn stage_input(input: &Path, staged: &Path) -> std::io::Result<()> {
    // Resolved first because a symlink stores its target verbatim: the link
    // is read from inside the staging directory, so a relative input — which
    // is what a build invoked from the package directory produces — would
    // dangle there and the archiver would report the staged name missing.
    let input = std::fs::canonicalize(input)?;
    std::os::unix::fs::symlink(input, staged)
}

/// Puts `input` at `staged` under its space-free name (copied).
#[cfg(not(unix))]
fn stage_input(input: &Path, staged: &Path) -> std::io::Result<()> {
    std::fs::copy(input, staged).map(|_| ())
}

/// The MRI script that merges the runtime archive and the library's object.
///
/// Built as its own function so the exact commands are assertable without
/// running an archiver, which is the only part of this that a machine with no
/// LLVM can check.
///
/// The names are bare: MRI scripts tokenize on whitespace and support no
/// quoting, so every path is staged under a space-free name in one directory
/// ([`archive_static_library`] runs the tool from there). A build directory
/// with a space in it — several Windows defaults have one — would otherwise
/// truncate into nonsense tokens.
fn mri_script(object: &str, runtime_archive: &str, archive: &str) -> String {
    format!("CREATE {archive}\nADDLIB {runtime_archive}\nADDMOD {object}\nSAVE\nEND\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_archive_script_splices_the_runtime_in_rather_than_nesting_it() {
        // `ADDLIB` takes the runtime archive's *members*; `ADDMOD` would nest
        // the archive inside the result, which links as nothing.
        let script = mri_script("object.o", "runtime.a", "out.a");
        assert_eq!(
            script,
            "CREATE out.a\n\
             ADDLIB runtime.a\n\
             ADDMOD object.o\n\
             SAVE\n\
             END\n"
        );
    }

    #[test]
    fn the_script_takes_bare_names_so_spaces_cannot_split_them() {
        let script = mri_script("My Project.object.o", "lib kira.a", "out put.a");
        assert!(script.contains("CREATE out put.a"));
    }
}
