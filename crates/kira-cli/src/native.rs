//! The native (LLVM) half of `build` and `run`: driving the backend, resolving
//! the foreign bindings a VM run needs, and executing a built program.
//!
//! Two questions each large enough to answer on their own sit beside it:
//! [`artifacts`] decides where a build's files go and what they are called, and
//! [`runtime`] finds the Kira runtime archive to link — which for a build aimed
//! at another machine is most of the work.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use kira_backend_api::NativeTarget;
use kira_debug::DebugInfo;
use kira_ir::IrProgram;
use kira_llvm_backend::{
    LlvmError, NativeArtifacts, NativeBuildOptions, NativeBuildTarget, NativeLinkInputs,
};
use kira_main::ForeignBindingTarget;

mod artifacts;
mod runtime;

pub use artifacts::Artifacts;
pub use runtime::{MissingCrossRuntimeArchive, hybrid_runtime_archive, runtime_archive};

/// Builds `program` into a native executable, linking `foreign_link`.
///
/// The archives are the selected C static libraries that satisfy the program's
/// `@FFI.Extern` imports; each generated adapter references its C symbol, so
/// naming them on the link line pulls in exactly the members those symbols need.
pub fn build(
    program: &IrProgram,
    source: &Path,
    emit_llvm_ir: bool,
    optimize: bool,
    foreign_link: &NativeLinkInputs,
    target: &NativeBuildTarget,
) -> Result<NativeArtifacts, NativeError> {
    // Asked before anything else a cross build needs is looked for. The runtime
    // archive and the sysroot are things a machine can be given; a code
    // generator this compiler was not linked with is not, so reporting the
    // arrangeable problems first would send the user off to arrange them for a
    // build that was never going to emit.
    kira_llvm_backend::supports_target(target.target())?;
    let artifacts = Artifacts::for_source_targeting(source, target)
        .map_err(|source| NativeError::Layout { source })?;
    let options = NativeBuildOptions {
        module_name: artifacts.stem().to_owned(),
        object_path: artifacts.object(),
        executable_path: Some(artifacts.executable_for(target)),
        // A whole-program native build has no second half to load.
        shared_library_path: None,
        // A program is entered at `main` and exports nothing.
        archive_path: None,
        exports: kira_llvm_backend::NativeExportSurface::default(),
        ir_path: emit_llvm_ir.then(|| artifacts.llvm_ir()),
        runtime_archive: runtime_archive(program, target.target())?,
        optimize,
        unavailable_imports: foreign_link.unavailable_imports().to_vec(),
        foreign_link: foreign_link.clone(),
        target: target.clone(),
    };
    Ok(kira_llvm_backend::build_native(program, &options)?)
}

/// Builds the whole native program library an LLVM live runner loads in
/// process, linking `foreign_link` and the Kira runtime into it.
pub fn build_live(
    program: &IrProgram,
    source: &Path,
    emit_llvm_ir: bool,
    optimize: bool,
    foreign_link: &NativeLinkInputs,
) -> Result<NativeArtifacts, NativeError> {
    let artifacts =
        Artifacts::for_source(source).map_err(|source| NativeError::Layout { source })?;
    let options = NativeBuildOptions {
        module_name: format!("{}_live", artifacts.stem()),
        object_path: artifacts.object(),
        executable_path: None,
        shared_library_path: Some(artifacts.live_library()),
        archive_path: None,
        exports: kira_llvm_backend::NativeExportSurface::default(),
        ir_path: emit_llvm_ir.then(|| artifacts.llvm_ir()),
        runtime_archive: runtime_archive(program, &NativeTarget::Host)?,
        optimize,
        unavailable_imports: foreign_link.unavailable_imports().to_vec(),
        foreign_link: foreign_link.clone(),
        // A live library is loaded into this process, so it is this machine's.
        target: NativeBuildTarget::host(),
    };
    Ok(kira_llvm_backend::build_native_live(program, &options)?)
}

/// Builds a native executable with debug metadata for a debugger session.
pub fn build_debug(
    program: &IrProgram,
    source: &Path,
    emit_llvm_ir: bool,
    optimize: bool,
    foreign_link: &NativeLinkInputs,
    debug: &DebugInfo,
) -> Result<NativeArtifacts, NativeError> {
    let artifacts =
        Artifacts::for_source(source).map_err(|source| NativeError::Layout { source })?;
    let options = NativeBuildOptions {
        module_name: artifacts.stem().to_owned(),
        object_path: artifacts.object(),
        executable_path: Some(artifacts.executable()),
        shared_library_path: None,
        archive_path: None,
        exports: kira_llvm_backend::NativeExportSurface::default(),
        ir_path: emit_llvm_ir.then(|| artifacts.llvm_ir()),
        runtime_archive: runtime_archive(program, &NativeTarget::Host)?,
        optimize,
        unavailable_imports: foreign_link.unavailable_imports().to_vec(),
        foreign_link: foreign_link.clone(),
        // A debugger session attaches to a process on this machine, so a debug
        // build is this machine's whatever else the invocation asked for.
        target: NativeBuildTarget::host(),
    };
    Ok(kira_llvm_backend::build_native_debug(
        program, &options, debug,
    )?)
}

/// Builds the shared carrier, when needed, and creates one direct Libffi
/// binding per foreign import.
pub fn direct_foreign_bindings(
    program: &IrProgram,
    source: &Path,
    foreign_link: &NativeLinkInputs,
) -> Result<Vec<kira_main::ForeignBinding>, NativeError> {
    let image_names: HashSet<&str> = foreign_link
        .image_libraries()
        .iter()
        .map(String::as_str)
        .collect();
    let carrier = if program.foreign_imports.is_empty() || foreign_link.static_archives().is_empty()
    {
        None
    } else {
        build_ffi_carrier_for_imports(program, source, foreign_link)?
    };
    Ok(program
        .foreign_imports
        .iter()
        .map(|entry| {
            let signature = entry.import.signature().clone();
            // Asked first, because every question below is about libraries and a
            // system call answers none of them: it has no library name, so the
            // lookup would miss and record the import as excluded on this target
            // — a call the host can serve, reported as one nothing can.
            if let Some(call) = entry.import.as_syscall() {
                return kira_main::ForeignBinding::syscall(call, signature);
            }
            let path = foreign_link
                .library_paths()
                .iter()
                .find(|(name, _)| name == entry.import.library())
                .map(|(_, path)| loadable_foreign_library_path(path, foreign_link));
            let binding = if entry.import.library() == kira_dynamic_ffi::HOST_RUNTIME_LIBRARY {
                kira_main::ForeignBinding::process(entry.import.symbol(), signature)
            } else if image_names.contains(&entry.import.library()) {
                match carrier.as_ref() {
                    Some(carrier) if carrier.symbols.contains(entry.import.symbol()) => {
                        kira_main::ForeignBinding::dynamic(
                            &carrier.path,
                            entry.import.symbol(),
                            signature,
                        )
                    }
                    // An image-resident row may also declare symbols supplied by
                    // the host executable or a system library the carrier pulls
                    // in — live-session telemetry, `objc_msgSend`. It must not
                    // become a carrier export merely because the row is linked
                    // in; the host process resolves it.
                    _ => kira_main::ForeignBinding::process(entry.import.symbol(), signature),
                }
            } else if let Some(path) = path {
                kira_main::ForeignBinding::dynamic(path, entry.import.symbol(), signature)
            } else {
                kira_main::ForeignBinding::unavailable(signature)
            };
            // An address import binds its symbol exactly as a call does -- the
            // branches above chose where -- and differs only in what happens
            // after the lookup, so it is marked here rather than in each of them.
            if entry.import.abi().answers_an_address() {
                binding.answering_address()
            } else {
                binding
            }
        })
        .collect())
}

/// Creates hybrid bindings, using its native half as the static-library carrier.
pub fn hybrid_foreign_bindings(
    program: &IrProgram,
    native_half: &Path,
    foreign_link: &NativeLinkInputs,
) -> Vec<kira_main::ForeignBinding> {
    let image_names: HashSet<&str> = foreign_link
        .image_libraries()
        .iter()
        .map(String::as_str)
        .collect();
    program
        .foreign_imports
        .iter()
        .map(|entry| {
            // The lookup below picks WHERE the symbol is bound, in several
            // early returns. An address import binds it the same way and differs
            // only in what happens after, so the choice is made once here rather
            // than repeated at each return.
            let binding = hybrid_binding_target(entry, foreign_link, &image_names, native_half);
            if entry.import.abi().answers_an_address() {
                binding.answering_address()
            } else {
                binding
            }
        })
        .collect()
}

/// Where one hybrid import's symbol is bound.
fn hybrid_binding_target(
    entry: &kira_ir::IrForeignImport,
    foreign_link: &NativeLinkInputs,
    image_names: &HashSet<&str>,
    native_half: &Path,
) -> kira_main::ForeignBinding {
    {
        {
            let signature = entry.import.signature().clone();
            // As in `direct_foreign_bindings`: a system call is not a library
            // lookup, and recording it as a failed one names an artifact that
            // was never meant to exist.
            if let Some(call) = entry.import.as_syscall() {
                return kira_main::ForeignBinding::syscall(call, signature);
            }
            let library = entry.import.library();
            if library == kira_dynamic_ffi::HOST_RUNTIME_LIBRARY {
                return kira_main::ForeignBinding::process(entry.import.symbol(), signature);
            }
            if image_names.contains(library) {
                return kira_main::ForeignBinding::dynamic(
                    native_half,
                    entry.import.symbol(),
                    signature,
                );
            }
            foreign_link
                .library_paths()
                .iter()
                .find(|(name, _)| name == library)
                .map(|(_, path)| {
                    kira_main::ForeignBinding::dynamic(
                        loadable_foreign_library_path(path, foreign_link),
                        entry.import.symbol(),
                        signature.clone(),
                    )
                })
                .unwrap_or_else(|| kira_main::ForeignBinding::unavailable(signature))
        }
    }
}

/// Returns explicit foreign library files that a native live bundle must carry.
///
/// Image-resident libraries — static archives, and the frameworks and system
/// libraries linked into the image — are not opened as load-time files. A shared
/// library remains an input to the direct Libffi call path, so it has to be
/// staged beside the live image.
pub(crate) fn dynamic_foreign_library_paths(foreign_link: &NativeLinkInputs) -> Vec<PathBuf> {
    let image_names: HashSet<&str> = foreign_link
        .image_libraries()
        .iter()
        .map(String::as_str)
        .collect();
    foreign_link
        .library_paths()
        .iter()
        .filter(|(name, path)| !image_names.contains(name.as_str()) && path.is_file())
        .map(|(_, path)| loadable_foreign_library_path(path, foreign_link))
        .collect()
}

/// Selects the file a direct loader can open for a resolved dynamic row.
///
/// MSVC native targets commonly record an import `.lib` for the link step and
/// a sibling `runtimeFiles` directory for the actual `.dll`. LibFFI opens the
/// latter; handing the import library to `LoadLibraryExW` is a link/load
/// category error that only appears once a VM live bundle starts.
fn loadable_foreign_library_path(path: &Path, foreign_link: &NativeLinkInputs) -> PathBuf {
    if !cfg!(target_env = "msvc") || path.extension().and_then(|ext| ext.to_str()) != Some("lib") {
        return path.to_path_buf();
    }
    let stem = path.file_stem();
    let direct = path.with_extension("dll");
    if direct.is_file() {
        return direct;
    }
    for declared in foreign_link.runtime_files() {
        if declared.is_file()
            && declared
                .file_stem()
                .zip(stem)
                .is_some_and(|(candidate, expected)| candidate == expected)
            && declared.extension().and_then(|ext| ext.to_str()) == Some("dll")
        {
            return declared.clone();
        }
        if declared.is_dir() {
            let Ok(entries) = std::fs::read_dir(declared) else {
                continue;
            };
            for entry in entries.flatten() {
                let candidate = entry.path();
                if candidate.extension().and_then(|ext| ext.to_str()) == Some("dll")
                    && candidate
                        .file_stem()
                        .zip(stem)
                        .is_some_and(|(candidate, expected)| candidate == expected)
                {
                    return candidate;
                }
            }
        }
    }
    path.to_path_buf()
}

/// Writes the resolved library path for each import in import order.
pub fn write_foreign_binding_paths(
    path: &Path,
    bindings: &[kira_main::ForeignBinding],
) -> Result<(), NativeError> {
    write_binding_manifest(path, bindings, false)
}

/// Writes import-ordered binding file names for a relocatable live bundle.
pub fn write_foreign_binding_names(
    path: &Path,
    bindings: &[kira_main::ForeignBinding],
) -> Result<(), NativeError> {
    write_binding_manifest(path, bindings, true)
}

fn write_binding_manifest(
    path: &Path,
    bindings: &[kira_main::ForeignBinding],
    names_only: bool,
) -> Result<(), NativeError> {
    let mut text = String::new();
    for binding in bindings {
        match &binding.target {
            ForeignBindingTarget::Library {
                path: library_path, ..
            } => {
                let rendered_path = if names_only {
                    library_path
                        .file_name()
                        .ok_or_else(|| NativeError::BindingManifest {
                            path: library_path.to_path_buf(),
                            message: "a foreign library path has no file name".to_owned(),
                        })?
                } else {
                    library_path.as_os_str()
                };
                let rendered = rendered_path.to_string_lossy();
                if rendered.contains(['\r', '\n']) {
                    return Err(NativeError::BindingManifest {
                        path: library_path.to_path_buf(),
                        message: "a foreign library path contains a line break".to_owned(),
                    });
                }
                text.push_str(&rendered);
            }
            ForeignBindingTarget::Process { .. } => {
                text.push_str(kira_dynamic_ffi::PROCESS_BINDING_MARKER);
            }
            // Neither writes a path, and neither needs a token of its own. A
            // process binding does because nothing else records it; a system
            // call does not, because the `.kbc` beside this manifest carries the
            // import's ABI and the reader rebuilds the binding from that. One
            // fact, in the one file that already had to carry it.
            ForeignBindingTarget::Unavailable | ForeignBindingTarget::Syscall { .. } => {}
        }
        text.push('\n');
    }
    std::fs::write(path, text).map_err(|source| NativeError::BindingManifest {
        path: path.to_path_buf(),
        message: source.to_string(),
    })
}

/// Reads the import-ordered library paths written for a prebuilt VM test.
pub fn read_foreign_binding_paths(path: &Path) -> Result<Vec<Option<PathBuf>>, NativeError> {
    let text = std::fs::read_to_string(path).map_err(|source| NativeError::BindingManifest {
        path: path.to_path_buf(),
        message: source.to_string(),
    })?;
    Ok(text
        .lines()
        .map(|line| (!line.is_empty()).then(|| PathBuf::from(line)))
        .collect())
}

/// Recognizes the reserved direct-binding token written for the host process.
pub(crate) fn is_process_binding_path(path: &Path) -> bool {
    path == Path::new(kira_dynamic_ffi::PROCESS_BINDING_MARKER)
}

/// Copies file-backed direct bindings beside a hybrid or live artifact and
/// rewrites their paths so the bundle remains relocatable.
pub fn stage_direct_foreign_bindings(
    destination_directory: &Path,
    bindings: &[kira_main::ForeignBinding],
) -> Result<Vec<kira_main::ForeignBinding>, NativeError> {
    std::fs::create_dir_all(destination_directory).map_err(|source| {
        NativeError::BindingManifest {
            path: destination_directory.to_path_buf(),
            message: format!("cannot prepare the staging directory: {source}"),
        }
    })?;
    let mut staged = Vec::with_capacity(bindings.len());
    let mut destinations = HashMap::new();
    for binding in bindings {
        let mut binding = binding.clone();
        let Some(path) = binding.library_path().map(Path::to_path_buf) else {
            staged.push(binding);
            continue;
        };
        if !path.is_file() {
            staged.push(binding);
            continue;
        }
        let Some(name) = path.file_name() else {
            return Err(NativeError::BindingManifest {
                path: path.clone(),
                message: "a foreign library path has no file name".to_owned(),
            });
        };
        let destination = destination_directory.join(name);
        if let Some(previous) = destinations.insert(destination.clone(), path.clone())
            && previous != path
        {
            return Err(NativeError::BindingManifest {
                path: destination,
                message: format!(
                    "foreign libraries `{}` and `{}` have the same file name",
                    previous.display(),
                    path.display()
                ),
            });
        }
        if path != destination {
            std::fs::copy(&path, &destination).map_err(|source| NativeError::BindingManifest {
                path: path.clone(),
                message: format!("cannot stage beside the artifact: {source}"),
            })?;
        }
        let ForeignBindingTarget::Library { symbol, .. } = binding.target else {
            staged.push(binding);
            continue;
        };
        binding.target = ForeignBindingTarget::Library {
            path: destination,
            symbol,
        };
        staged.push(binding);
    }
    Ok(staged)
}

/// The carrier path and the archive-owned symbols it actually exports.
struct BuiltFfiCarrier {
    path: PathBuf,
    symbols: HashSet<String>,
}

/// Builds a carrier only when at least one requested symbol is defined by a
/// selected static archive.
fn build_ffi_carrier_for_imports(
    program: &IrProgram,
    source: &Path,
    foreign_link: &NativeLinkInputs,
) -> Result<Option<BuiltFfiCarrier>, NativeError> {
    let artifacts =
        Artifacts::for_source(source).map_err(|source| NativeError::Layout { source })?;
    let static_names: HashSet<&str> = foreign_link
        .static_archives()
        .iter()
        .map(|(name, _)| name.as_str())
        .collect();
    let symbols: Vec<String> = program
        .foreign_imports
        .iter()
        .filter(|entry| static_names.contains(entry.import.library()))
        .map(|entry| entry.import.symbol().to_owned())
        .collect();
    if symbols.is_empty() {
        return Ok(None);
    }
    let llvm = kira_toolchain::discover(None).map_err(LlvmError::from)?;
    let path = artifacts.ffi_carrier();
    let retained = kira_llvm_backend::link_ffi_carrier(&llvm, foreign_link, &symbols, &path)
        .map_err(LlvmError::from)?;
    if retained.is_empty() {
        return Ok(None);
    }
    Ok(Some(BuiltFfiCarrier {
        path,
        symbols: retained.into_iter().collect(),
    }))
}

/// Runs a built native executable with the program arguments, returning its
/// exit code.
///
/// The child inherits this process's streams, so a native run's output is
/// indistinguishable from a VM run's.
///
/// `quit_after` ends the child at a deadline. It is enforced here rather than
/// by the caller because the program is a *child* on this backend: a caller
/// that ended itself would leave the program running with nothing waiting on
/// it, which is the window nobody can close.
pub fn execute(
    executable: &Path,
    arguments: &[String],
    quit_after: Option<Duration>,
) -> Result<i32, NativeError> {
    let mut child = std::process::Command::new(executable)
        .args(arguments)
        .spawn()
        .map_err(|source| NativeError::Spawn {
            executable: executable.to_path_buf(),
            source,
        })?;
    let mut ended_at_deadline = false;
    let status = match quit_after {
        None => child.wait(),
        Some(bound) => {
            /// How often to re-check whether the program has exited.
            const POLL: Duration = Duration::from_millis(5);

            let deadline = std::time::Instant::now() + bound;
            loop {
                match child.try_wait() {
                    Ok(Some(status)) => break Ok(status),
                    Err(error) => break Err(error),
                    Ok(None) if std::time::Instant::now() >= deadline => {
                        crate::progress::out!("kira run: --quit-after reached");
                        ended_at_deadline = true;
                        let _ = child.kill();
                        break child.wait();
                    }
                    Ok(None) => std::thread::sleep(POLL),
                }
            }
        }
    }
    .map_err(|source| NativeError::Spawn {
        executable: executable.to_path_buf(),
        source,
    })?;
    if ended_at_deadline {
        // The status is the kill's, not the program's: every host reports one
        // (Windows an exit code, unix a signal) and neither says anything about
        // the run, which did exactly what it was asked.
        return Ok(crate::pipeline::EXIT_OK);
    }
    // A signal-killed child reports no code; surface it as a failure rather
    // than as success.
    Ok(status.code().unwrap_or(1))
}

/// Why a native build or run failed.
#[derive(Debug, thiserror::Error)]
pub enum NativeError {
    /// The artifact directory could not be prepared.
    #[error("cannot create the `.kira-build` directory: {source}")]
    Layout {
        /// The underlying I/O failure.
        #[source]
        source: std::io::Error,
    },
    /// The runtime archive could not be located.
    #[error("cannot locate the native runtime archive: {source}")]
    RuntimeArchive {
        /// The underlying I/O failure.
        #[source]
        source: std::io::Error,
    },
    /// No runtime archive built for the requested cross target was found.
    ///
    /// Its own variant rather than the missing-file error the linker would
    /// raise, because the fix is one specific command with one specific target,
    /// and "`libkira_native_bridge.a` is missing" — said while this machine's
    /// copy sits plainly in the same directory — is the least useful true thing
    /// the compiler could say.
    ///
    /// Boxed because it carries six fields to say all that, and an enum is as
    /// large as its largest variant: unboxed it made every `Result` in this
    /// module, and in the two above it, 136 bytes wide for the sake of the one
    /// path that fails.
    #[error(transparent)]
    CrossRuntimeArchive(Box<MissingCrossRuntimeArchive>),
    /// The backend failed.
    #[error(transparent)]
    Backend(#[from] LlvmError),
    /// A prebuilt VM binding manifest could not be written or read.
    #[error("cannot use foreign binding manifest `{path}`: {message}")]
    BindingManifest {
        /// The binding manifest path.
        path: PathBuf,
        /// Why the manifest was rejected.
        message: String,
    },
    /// The built executable could not be started.
    #[error("cannot run `{executable}`: {source}")]
    Spawn {
        /// The executable that could not be started.
        executable: PathBuf,
        /// The underlying I/O failure.
        #[source]
        source: std::io::Error,
    },
}
