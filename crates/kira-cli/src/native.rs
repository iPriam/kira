//! The native (LLVM) half of `build` and `run`: artifact layout, locating the
//! runtime archive, and executing a built program.
//!
//! [`Artifacts`] is where every backend's output paths are decided, Web
//! included — one program has one build directory, whatever it was built for.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use kira_debug::DebugInfo;
use kira_ir::IrProgram;
use kira_llvm_backend::{LlvmError, NativeArtifacts, NativeBuildOptions, NativeLinkInputs};
use kira_main::ForeignBindingTarget;

/// Where a program's build artifacts live: its package's `.kira-build/`
/// ([`kira_project::build_directory`]).
///
/// Artifacts stay inside the package they came from rather than in a shared
/// location, so two *programs* can never race for one output path. Two builds
/// of the **same** program still can — the names are the program's, not the
/// builder's — which is what the lock below is for: holding it for the life of
/// this value makes a second builder wait rather than relink an executable the
/// first one is still writing.
pub struct Artifacts {
    /// The `.kira-build` directory itself.
    directory: PathBuf,
    /// The source file's stem, which every artifact is named after.
    stem: String,
    /// Held for as long as these artifacts are being written.
    ///
    /// Never read. It exists to be dropped at the end of the build, which is
    /// when the directory becomes another builder's to use.
    _lock: crate::build_lock::BuildLock,
}

impl Artifacts {
    /// Resolves the artifact layout for `source`, creating the directory.
    pub fn for_source(source: &Path) -> Result<Self, std::io::Error> {
        let directory = kira_project::build_directory(source);
        // Creates the directory as well as locking it, so a caller never has
        // one without the other.
        let lock = crate::build_lock::BuildLock::acquire(&directory)?;
        let stem = source
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_else(|| "program".to_owned());
        Ok(Artifacts {
            directory,
            stem,
            _lock: lock,
        })
    }

    /// The object file path.
    pub fn object(&self) -> PathBuf {
        self.directory.join(format!("{}.o", self.stem))
    }

    /// The native executable path.
    ///
    /// Through `executable_name`, so the file carries the extension its host
    /// needs to run it: Windows will not execute a PE by a name with no `.exe`,
    /// and this wrote the bare stem on every platform. The release gate that
    /// builds a program and runs the result is what surfaced it — the binary
    /// was there, under a name nothing could launch.
    pub fn executable(&self) -> PathBuf {
        self.directory
            .join(kira_toolchain::executable_name(&self.stem))
    }

    /// The textual LLVM IR dump path.
    pub fn llvm_ir(&self) -> PathBuf {
        self.directory.join(format!("{}.ll", self.stem))
    }

    /// The directory holding the Web artifacts, which is what `run` serves.
    ///
    /// Separate from the rest because it is exposed over HTTP: only what a
    /// browser needs belongs under a served root.
    pub fn web_directory(&self) -> PathBuf {
        self.directory.join("web")
    }

    /// The source file's stem, which every artifact is named after.
    pub fn stem(&self) -> &str {
        &self.stem
    }

    /// The directory holding this source's finished artifacts.
    pub fn directory(&self) -> &Path {
        &self.directory
    }

    /// The hybrid manifest path: the artifact a hybrid run loads first.
    pub fn manifest(&self) -> PathBuf {
        self.directory.join(format!("{}.khm", self.stem))
    }

    /// The bytecode payload path, for a hybrid build.
    pub fn bytecode(&self) -> PathBuf {
        self.directory.join(format!("{}.kbc", self.stem))
    }

    /// The shared library path holding a hybrid program's native half.
    ///
    /// Named the way the host platform's loader expects, so `dlopen` finds it
    /// by the name the manifest records.
    pub fn shared_library(&self) -> PathBuf {
        self.directory.join(shared_library_name(&self.stem))
    }

    /// The whole-program native library path used by an LLVM live session.
    pub fn live_library(&self) -> PathBuf {
        self.directory
            .join(shared_library_name(&format!("{}_live", self.stem)))
    }

    /// The cache marker for a VM live session's reusable native adapter surface.
    pub fn native_surface_key(&self) -> PathBuf {
        self.directory.join(format!("{}.native-surface", self.stem))
    }

    /// The shared carrier for static foreign archives.
    pub fn ffi_carrier(&self) -> PathBuf {
        self.directory
            .join(shared_library_name(&format!("{}_ffi_carrier", self.stem)))
    }

    /// The import-ordered direct Libffi binding manifest used by a live VM.
    pub fn foreign_bindings(&self) -> PathBuf {
        self.directory.join(format!("{}.ffi-bindings", self.stem))
    }
}

/// The platform's file name for a shared library called `stem`.
fn shared_library_name(stem: &str) -> String {
    let extension = if cfg!(target_os = "macos") {
        "dylib"
    } else if cfg!(target_os = "windows") {
        "dll"
    } else {
        "so"
    };
    if cfg!(target_os = "windows") {
        format!("{stem}.{extension}")
    } else {
        format!("lib{stem}.{extension}")
    }
}

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
) -> Result<NativeArtifacts, NativeError> {
    let artifacts =
        Artifacts::for_source(source).map_err(|source| NativeError::Layout { source })?;
    let options = NativeBuildOptions {
        module_name: artifacts.stem.clone(),
        object_path: artifacts.object(),
        executable_path: Some(artifacts.executable()),
        // A whole-program native build has no second half to load.
        shared_library_path: None,
        // A program is entered at `main` and exports nothing.
        archive_path: None,
        exports: kira_llvm_backend::NativeExportSurface::default(),
        ir_path: emit_llvm_ir.then(|| artifacts.llvm_ir()),
        runtime_archive: runtime_archive(program)?,
        optimize,
        unavailable_imports: foreign_link.unavailable_imports().to_vec(),
        foreign_link: foreign_link.clone(),
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
        module_name: format!("{}_live", artifacts.stem),
        object_path: artifacts.object(),
        executable_path: None,
        shared_library_path: Some(artifacts.live_library()),
        archive_path: None,
        exports: kira_llvm_backend::NativeExportSurface::default(),
        ir_path: emit_llvm_ir.then(|| artifacts.llvm_ir()),
        runtime_archive: runtime_archive(program)?,
        optimize,
        unavailable_imports: foreign_link.unavailable_imports().to_vec(),
        foreign_link: foreign_link.clone(),
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
        module_name: artifacts.stem.clone(),
        object_path: artifacts.object(),
        executable_path: Some(artifacts.executable()),
        shared_library_path: None,
        archive_path: None,
        exports: kira_llvm_backend::NativeExportSurface::default(),
        ir_path: emit_llvm_ir.then(|| artifacts.llvm_ir()),
        runtime_archive: runtime_archive(program)?,
        optimize,
        unavailable_imports: foreign_link.unavailable_imports().to_vec(),
        foreign_link: foreign_link.clone(),
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
    let static_names: HashSet<&str> = foreign_link
        .static_archives()
        .iter()
        .map(|(name, _)| name.as_str())
        .collect();
    let carrier = if program.foreign_imports.is_empty() || static_names.is_empty() {
        None
    } else {
        build_ffi_carrier_for_imports(program, source, foreign_link)?
    };
    Ok(program
        .foreign_imports
        .iter()
        .map(|entry| {
            let signature = entry.import.signature().clone();
            let path = foreign_link
                .library_paths()
                .iter()
                .find(|(name, _)| name == entry.import.library())
                .map(|(_, path)| loadable_foreign_library_path(path, foreign_link));
            if entry.import.library() == kira_dynamic_ffi::HOST_RUNTIME_LIBRARY {
                kira_main::ForeignBinding::process(entry.import.symbol(), signature)
            } else if static_names.contains(&entry.import.library()) {
                match carrier.as_ref() {
                    Some(carrier) if carrier.symbols.contains(entry.import.symbol()) => {
                        kira_main::ForeignBinding::dynamic(
                            &carrier.path,
                            entry.import.symbol(),
                            signature,
                        )
                    }
                    // A static-library row may also declare symbols supplied by
                    // the host executable, such as live-session telemetry. It
                    // must not become a carrier export merely because the row
                    // contains an archive.
                    _ => kira_main::ForeignBinding::process(entry.import.symbol(), signature),
                }
            } else if let Some(path) = path {
                kira_main::ForeignBinding::dynamic(path, entry.import.symbol(), signature)
            } else {
                kira_main::ForeignBinding::unavailable(signature)
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
    let static_names: HashSet<&str> = foreign_link
        .static_archives()
        .iter()
        .map(|(name, _)| name.as_str())
        .collect();
    program
        .foreign_imports
        .iter()
        .map(|entry| {
            let signature = entry.import.signature().clone();
            let library = entry.import.library();
            if library == kira_dynamic_ffi::HOST_RUNTIME_LIBRARY {
                return kira_main::ForeignBinding::process(entry.import.symbol(), signature);
            }
            if static_names.contains(library) {
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
        })
        .collect()
}

/// Returns explicit foreign library files that a native live bundle must carry.
///
/// Static archives are linked into the native image and do not need to survive
/// as load-time files. A shared library remains an input to the direct Libffi
/// call path, so it has to be staged beside the live image.
pub(crate) fn dynamic_foreign_library_paths(foreign_link: &NativeLinkInputs) -> Vec<PathBuf> {
    let static_names: HashSet<&str> = foreign_link
        .static_archives()
        .iter()
        .map(|(name, _)| name.as_str())
        .collect();
    foreign_link
        .library_paths()
        .iter()
        .filter(|(name, path)| !static_names.contains(name.as_str()) && path.is_file())
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
            ForeignBindingTarget::Unavailable => {}
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

/// Locates the native runtime archive `program` needs.
///
/// Two archives, and the program picks. The base one carries the runtime every
/// native program needs; `libkira_compiler_bridge.a` carries that *and* the
/// check-only frontend, because native code has no host to ask for a compiler
/// and can only reach one that was linked in. Linking the larger one always
/// would put a compiler inside every program Kira ever produces, and linking
/// both is not possible — two Rust static libraries in one link line duplicate
/// the standard library — so the answer is whichever one this program needs.
///
/// Cargo writes a workspace member's staticlib beside the executable, while a
/// package-only build may leave a hashed copy under `target/<profile>/deps/`.
/// Accept both layouts so `cargo build -p kira-cli` and a workspace build have
/// the same runtime behavior.
pub fn runtime_archive(program: &IrProgram) -> Result<PathBuf, NativeError> {
    runtime_archive_for(program.uses_compiler())
}

/// Locates the runtime archive needed by an application hybrid half.
///
/// Only compiler expressions in reachable native bodies require the compiler
/// bridge; runtime-owned compiler calls stay in the VM half.
pub fn hybrid_runtime_archive(program: &IrProgram) -> Result<PathBuf, NativeError> {
    runtime_archive_for(kira_llvm_backend::hybrid_uses_compiler_runtime(program))
}

fn runtime_archive_for(uses_compiler: bool) -> Result<PathBuf, NativeError> {
    let executable =
        std::env::current_exe().map_err(|source| NativeError::RuntimeArchive { source })?;
    let directory = executable
        .parent()
        .ok_or_else(|| NativeError::RuntimeArchive {
            source: std::io::Error::other("this executable has no parent directory"),
        })?;
    let name = archive_file_name(uses_compiler);
    Ok(find_runtime_archive(directory, name).unwrap_or_else(|| directory.join(name)))
}

/// Finds an un-hashed profile artifact first, then the newest hashed staticlib
/// Cargo placed in `deps/` when the runtime was built as a dependency.
fn find_runtime_archive(directory: &Path, name: &str) -> Option<PathBuf> {
    let direct = directory.join(name);
    if direct.is_file() {
        return Some(direct);
    }

    let expected = Path::new(name);
    let stem = expected.file_stem()?.to_str()?;
    let extension = expected.extension()?.to_str()?;
    let prefix = format!("{stem}-");
    let dependencies = directory.join("deps");
    let mut candidates: Vec<PathBuf> = std::fs::read_dir(dependencies)
        .ok()?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.is_file()
                && path.extension().and_then(|value| value.to_str()) == Some(extension)
                && path
                    .file_stem()
                    .and_then(|value| value.to_str())
                    .is_some_and(|value| value.starts_with(&prefix))
        })
        .collect();
    candidates.sort_by_key(|path| {
        std::fs::metadata(path)
            .and_then(|metadata| metadata.modified())
            .ok()
    });
    candidates.pop()
}

/// Which archive file a program needs, by name.
///
/// Split from the path so a test can assert the choice without a built `kira`
/// beside it.
///
/// The spelling is the host toolchain's, because the file being named is one
/// cargo just wrote for this host: MSVC writes `<name>.lib` and everything else
/// writes `lib<name>.a`. Naming it the Unix way on Windows looks for a file
/// cargo never produced, which is the "native runtime archive is missing" error
/// with nothing missing.
fn archive_file_name(uses_compiler: bool) -> &'static str {
    let crate_name = match uses_compiler {
        true => "kira_compiler_bridge",
        false => "kira_native_bridge",
    };
    match (crate_name, cfg!(target_env = "msvc")) {
        ("kira_compiler_bridge", true) => "kira_compiler_bridge.lib",
        ("kira_compiler_bridge", false) => "libkira_compiler_bridge.a",
        (_, true) => "kira_native_bridge.lib",
        (_, false) => "libkira_native_bridge.a",
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A program that never checks a package links the small archive; one that
    /// does links the archive that carries a compiler. Both, never neither and
    /// never both — two Rust static libraries in one link line do not link.
    #[test]
    fn the_archive_a_program_links_follows_from_whether_it_checks_packages() {
        // Asserted against the host's own spelling: the name has to be the one
        // cargo wrote next to this binary, so a test that pinned the Unix name
        // everywhere would pass on the platform where the name is wrong.
        if cfg!(target_env = "msvc") {
            assert_eq!(archive_file_name(false), "kira_native_bridge.lib");
            assert_eq!(archive_file_name(true), "kira_compiler_bridge.lib");
        } else {
            assert_eq!(archive_file_name(false), "libkira_native_bridge.a");
            assert_eq!(archive_file_name(true), "libkira_compiler_bridge.a");
        }
    }

    #[test]
    fn a_dependency_build_can_fall_back_to_a_hashed_profile_archive() {
        let directory = std::env::temp_dir().join(format!(
            "kira-runtime-archive-fallback-{}",
            std::process::id()
        ));
        let dependencies = directory.join("deps");
        std::fs::create_dir_all(&dependencies).expect("runtime archive test directory");
        let name = archive_file_name(false);
        let expected = Path::new(name);
        let hashed_name = format!(
            "{}-testhash.{}",
            expected
                .file_stem()
                .expect("runtime archive has a file stem")
                .to_string_lossy(),
            expected
                .extension()
                .expect("runtime archive has an extension")
                .to_string_lossy()
        );
        let hashed = dependencies.join(hashed_name);
        std::fs::write(&hashed, b"archive").expect("hashed runtime archive");

        assert_eq!(find_runtime_archive(&directory, name), Some(hashed.clone()));
        std::fs::remove_dir_all(directory).expect("remove runtime archive test directory");
    }

    #[test]
    fn artifacts_live_beside_their_source_and_share_its_stem() {
        let directory = std::env::temp_dir().join("kira-artifacts-test");
        let source = directory.join("hello.kira");
        std::fs::create_dir_all(&directory).expect("temp dir");

        let artifacts = Artifacts::for_source(&source).expect("layout");
        assert!(artifacts.object().ends_with(".kira-build/hello.o"));
        assert!(artifacts.executable().ends_with(format!(
            ".kira-build/{}",
            kira_toolchain::executable_name("hello")
        )));
        assert!(artifacts.llvm_ir().ends_with(".kira-build/hello.ll"));
        assert!(artifacts.object().starts_with(&directory));

        std::fs::remove_dir_all(&directory).ok();
    }
}
