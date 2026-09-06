//! The hybrid half of `build` and `run`: emitting a bundle, and running it.
//!
//! A hybrid build writes three artifacts into `.kira-build/`, from one IR:
//!
//! - `<stem>.kbc` — the bytecode half, every non-`@Native` function,
//! - `lib<stem>.dylib` (or `.so`) — the native half, one trampoline per
//!   reachable `@Native` function,
//! - `<stem>.khm` — the manifest tying them together, which a run loads first.
//!
//! # Where the manifest is built, and why not here
//!
//! In `kira-build`, alongside the hybrid *library* build, which needs the
//! identical description of the identical program. Two spellings of "describe
//! this program as a `.khm`" would be one too many — the manifest is what every
//! crossing marshals against, and a disagreement between an application's and a
//! library's is precisely the class of bug the runtime's bundle validation
//! exists to catch. So this module composes artifacts and paths, and asks
//! [`kira_build::hybrid_manifest`] what the program is.
//!
//! # Agreeing with both halves
//!
//! Every engine assignment resolves `Inherited` against
//! [`Execution::Runtime`](kira_runtime_abi::Execution::Runtime), exactly as
//! `kira_bytecode::compile_hybrid` and the LLVM backend's `build_hybrid` do. The
//! three must agree function for function.

use std::path::{Path, PathBuf};

use kira_debug::DebugInfo;
use kira_hybrid_definition::HybridManifest;
use kira_ir::IrProgram;
use kira_llvm_backend::{NativeBuildOptions, NativeLinkInputs};

use crate::native::{self, Artifacts, NativeError};

/// The artifacts a hybrid build produced.
pub struct HybridBundle {
    /// The manifest a run loads first.
    pub manifest: PathBuf,
    /// Explicit foreign library files staged beside the hybrid artifacts.
    ///
    /// The manifest records these paths for the VM half. A live bundle must
    /// carry the files as native dependency payloads as well, or the runner
    /// would receive a manifest that names libraries it never received.
    pub foreign_dependencies: Vec<PathBuf>,
}

/// Why a hybrid build or run failed.
#[derive(Debug, thiserror::Error)]
pub enum HybridError {
    /// The native half of the build failed.
    #[error(transparent)]
    Native(#[from] NativeError),
    /// The bytecode half of the build failed.
    #[error("bytecode compilation failed: {0}")]
    Bytecode(#[from] kira_bytecode::CompileError),
    /// The LLVM backend failed.
    #[error(transparent)]
    Backend(#[from] kira_llvm_backend::LlvmError),
    /// An artifact could not be written.
    #[error("cannot write `{path}`: {source}")]
    Io {
        /// The path being written.
        path: PathBuf,
        /// The underlying I/O failure.
        #[source]
        source: std::io::Error,
    },
    /// The program could not be described as a manifest.
    #[error(transparent)]
    Describe(#[from] kira_build::HybridLibraryError),
    /// Loading or running the bundle failed.
    #[error(transparent)]
    Runtime(#[from] kira_hybrid_runtime::HybridError),
    /// The managed sanitizer runtime could not be installed in a child.
    #[error(transparent)]
    SanitizerLaunch(#[from] kira_hybrid_launcher::SanitizerLaunchError),
    /// The CLI executable could not be located for a sanitized child.
    #[error("cannot locate the Kira executable for the sanitized hybrid child: {0}")]
    CurrentExecutable(std::io::Error),
    /// A sanitized hybrid child could not be started.
    #[error("cannot start the sanitized hybrid child: {0}")]
    SpawnSanitized(std::io::Error),
    /// A sanitized hybrid child was ended by the host without an exit code.
    #[error("the sanitized hybrid child ended without an exit code")]
    SanitizedChildSignal,
}

impl HybridError {
    /// Reports this failure the way the engine that produced it would.
    ///
    /// A trap is the program failing, and every other engine says so in the
    /// same words: `kira: runtime trap: …` on the VM and on native alike.
    /// Everything else here is the *bundle* failing — a manifest that will not
    /// decode, a native half that will not load — which is not a runtime trap
    /// and must not read as one.
    pub(crate) fn report(&self) {
        match self {
            HybridError::Runtime(kira_hybrid_runtime::HybridError::Trap(trap)) => {
                crate::progress::err!("kira: runtime trap: {trap}");
            }
            other => crate::progress::err!("kira: {other}"),
        }
    }
}

/// Builds `program` into a hybrid bundle under `.kira-build/`.
///
/// `foreign_link` are the selected C static libraries. They are linked into
/// the one native half — alongside the adapters the backend emits there — so the
/// session binds every foreign call out of a single dylib.
pub fn build(
    program: &IrProgram,
    source: &Path,
    emit_llvm_ir: bool,
    sanitize: kira_llvm_backend::Sanitize,
    foreign_link: &NativeLinkInputs,
) -> Result<HybridBundle, HybridError> {
    build_inner(program, source, emit_llvm_ir, sanitize, foreign_link, None)
}

/// Builds a hybrid bundle with debug metadata on its native half.
pub fn build_debug(
    program: &IrProgram,
    source: &Path,
    emit_llvm_ir: bool,
    foreign_link: &NativeLinkInputs,
    debug: &DebugInfo,
) -> Result<HybridBundle, HybridError> {
    build_inner(
        program,
        source,
        emit_llvm_ir,
        kira_llvm_backend::Sanitize::None,
        foreign_link,
        Some(debug),
    )
}

fn build_inner(
    program: &IrProgram,
    source: &Path,
    emit_llvm_ir: bool,
    sanitize: kira_llvm_backend::Sanitize,
    foreign_link: &NativeLinkInputs,
    debug: Option<&DebugInfo>,
) -> Result<HybridBundle, HybridError> {
    // The bytecode half: every function that is not `@Native`.
    let module = kira_bytecode::compile_hybrid(program)?;
    let artifacts =
        Artifacts::for_source(source).map_err(|source| NativeError::Layout { source })?;
    let bytecode_path = artifacts.bytecode();
    write(&bytecode_path, &module.to_bytes())?;

    // A hybrid bundle with no reachable native body can reuse its native half
    // when the crossing surface is unchanged.
    let reusable_native = !kira_llvm_backend::has_reachable_hybrid_native_functions(program);
    let surface_key = native_surface_key(program, foreign_link, sanitize);
    let cache_path = artifacts.native_surface_key();
    let cache_hit = debug.is_none()
        && reusable_native
        && artifacts.shared_library().is_file()
        && (!emit_llvm_ir || artifacts.llvm_ir().is_file())
        && std::fs::read_to_string(&cache_path)
            .map(|cached| cached == surface_key)
            .unwrap_or(false);
    let exports = if cache_hit {
        Vec::new()
    } else {
        // The native half: one trampoline per `@Native` function, plus one
        // adapter per foreign import, with the selected C archives linked in.
        let options = NativeBuildOptions {
            module_name: artifacts.stem().to_owned(),
            object_path: artifacts.object(),
            // A hybrid program has no entrypoint of its own to link: the host is
            // the executable, and this half is a library it loads.
            executable_path: None,
            shared_library_path: Some(artifacts.shared_library()),
            // A hybrid half is entered through its per-function trampolines, not
            // through an export surface, and it is a dylib rather than an archive.
            archive_path: None,
            exports: kira_llvm_backend::NativeExportSurface::default(),
            ir_path: emit_llvm_ir.then(|| artifacts.llvm_ir()),
            runtime_archive: native::hybrid_runtime_archive(program)?,
            optimize: debug.is_some_and(|debug| debug.optimized),
            unavailable_imports: foreign_link.unavailable_imports().to_vec(),
            foreign_link: foreign_link.clone(),
            // The interpreter running in this process opens this half, so it is
            // this machine's; a hybrid program has no second machine to be
            // split across.
            target: kira_llvm_backend::NativeBuildTarget::host(),
            sanitize,
        };
        let native = match debug {
            Some(debug) => kira_llvm_backend::build_hybrid_library_debug(program, &options, debug)?,
            None => kira_llvm_backend::build_hybrid_library(program, &options)?,
        };
        if reusable_native {
            std::fs::write(&cache_path, &surface_key).map_err(|source| HybridError::Io {
                path: cache_path.clone(),
                source,
            })?;
        }
        native.exports
    };

    let direct_bindings =
        native::hybrid_foreign_bindings(program, &artifacts.shared_library(), foreign_link);
    let direct_bindings =
        native::stage_direct_foreign_bindings(artifacts.directory(), &direct_bindings)?;
    let foreign_dependencies = direct_bindings
        .iter()
        .filter_map(|binding| binding.library_path().map(Path::to_path_buf))
        .filter(|path| path.is_file() && path != &artifacts.shared_library())
        .collect();
    let foreign_paths: Vec<Option<String>> = direct_bindings
        .iter()
        .map(|binding| match &binding.target {
            kira_main::ForeignBindingTarget::Library { path, .. } => Some(file_name(path)),
            kira_main::ForeignBindingTarget::Process { .. } => {
                Some(kira_dynamic_ffi::PROCESS_BINDING_MARKER.to_owned())
            }
            // No path, and none is missing: the manifest beside this list
            // carries each import's ABI, so the session knows a system call
            // from a library that resolved to nothing.
            kira_main::ForeignBindingTarget::Unavailable
            | kira_main::ForeignBindingTarget::Syscall { .. } => None,
        })
        .collect();

    // The manifest, last: it names the two payloads above.
    let manifest = manifest(
        program,
        &artifacts,
        &exports,
        kira_build::hybrid_internal_function_count(program, &module)?,
        &foreign_paths,
        sanitize,
    )?;
    let manifest_path = artifacts.manifest();
    write(&manifest_path, &manifest.to_bytes())?;

    Ok(HybridBundle {
        manifest: manifest_path,
        foreign_dependencies,
    })
}

/// The exact input that can change an all-runtime hybrid session's native library.
///
/// The runtime marker makes an old compiler's `.kira-build` output conservative
/// after a native ABI change. The ordinary Kira function bodies are deliberately
/// not present: they do not enter the native half of a VM bundle.
fn native_surface_key(
    program: &IrProgram,
    foreign_link: &NativeLinkInputs,
    sanitize: kira_llvm_backend::Sanitize,
) -> String {
    format!(
        "kira-vm-native-surface-v3\nruntime-abi={}\nimports={:?}\naggregates={:?}\ncallbacks={:?}\nlink={:?}\nsanitize={:?}",
        kira_runtime_abi::RUNTIME_ABI_MARKER,
        program.foreign_imports,
        program.foreign_aggregates,
        program.foreign_callbacks,
        foreign_link,
        sanitize,
    )
}

/// Builds a hybrid bundle and runs it, returning the program's exit code.
pub fn run(
    program: &IrProgram,
    source: &Path,
    emit_llvm_ir: bool,
    sanitize: kira_llvm_backend::Sanitize,
    foreign_link: &NativeLinkInputs,
    program_arguments: &[String],
) -> Result<i32, HybridError> {
    let bundle = build(program, source, emit_llvm_ir, sanitize, foreign_link)?;
    run_bundle(&bundle.manifest, program_arguments)
}

/// Runs an already-built hybrid bundle in an action child.
pub fn run_bundle(manifest: &Path, program_arguments: &[String]) -> Result<i32, HybridError> {
    if !kira_hybrid_launcher::needs_sanitizer_child(manifest)? {
        return run_bundle_in_process(manifest, program_arguments);
    }
    let executable = std::env::current_exe().map_err(HybridError::CurrentExecutable)?;
    let mut child = std::process::Command::new(executable);
    child
        .arg("__hybrid-run-host")
        .arg(manifest)
        .args(program_arguments);
    if kira_hybrid_launcher::configure_sanitizer_child(&mut child, manifest)? {
        let status = child.status().map_err(HybridError::SpawnSanitized)?;
        return status.code().ok_or(HybridError::SanitizedChildSignal);
    }
    run_bundle_in_process(manifest, program_arguments)
}

fn run_bundle_in_process(
    manifest: &Path,
    program_arguments: &[String],
) -> Result<i32, HybridError> {
    let session = kira_hybrid_runtime::Session::load(manifest)?;
    // SAFETY: the CLI owns this run boundary and does not access the process
    // environment from another thread while the Hybrid session executes.
    unsafe { kira_runtime_abi::env::with_arguments(program_arguments, || session.run())? };
    Ok(0)
}

/// Runs the hidden host side of a sanitized hybrid invocation.
pub(crate) fn run_host(args: &[String]) -> i32 {
    let Some((manifest, program_arguments)) = args.split_first() else {
        crate::progress::err!("kira: the hybrid run host needs a manifest path");
        return crate::pipeline::EXIT_USAGE;
    };
    match run_bundle(Path::new(manifest), program_arguments) {
        Ok(code) => code,
        Err(error) => {
            error.report();
            crate::pipeline::EXIT_FAILURE
        }
    }
}

/// Describes `program` as a manifest, given the trampolines the backend
/// exported.
///
/// Delegated rather than derived: `kira-build` describes a hybrid program for
/// the library build too, and one program must not have two descriptions.
fn manifest(
    program: &IrProgram,
    artifacts: &Artifacts,
    exports: &[(u32, String)],
    internal_functions: u32,
    foreign_paths: &[Option<String>],
    sanitize: kira_llvm_backend::Sanitize,
) -> Result<HybridManifest, HybridError> {
    let sanitizer = match sanitize {
        kira_llvm_backend::Sanitize::None => kira_hybrid_definition::HybridSanitizer::None,
        kira_llvm_backend::Sanitize::Address => kira_hybrid_definition::HybridSanitizer::Address,
    };
    let bytecode_file = file_name(&artifacts.bytecode());
    let native_file = file_name(&artifacts.shared_library());
    Ok(kira_build::hybrid_manifest_with_options(
        program,
        kira_build::HybridManifestOptions {
            module_name: artifacts.stem(),
            bytecode_file: &bytecode_file,
            native_file: &native_file,
            exports,
            internal_functions,
            foreign_paths,
            sanitizer,
        },
    )?)
}

/// The file name of `path`, which is how a manifest records a payload.
///
/// Recorded relative rather than absolute so the bundle can be moved as a unit;
/// the runtime resolves each against the manifest's own directory, and all
/// three artifacts share one.
fn file_name(path: &Path) -> String {
    path.file_name()
        .unwrap_or(path.as_os_str())
        .to_string_lossy()
        .into_owned()
}

/// Writes an artifact, naming it on failure.
fn write(path: &Path, bytes: &[u8]) -> Result<(), HybridError> {
    std::fs::write(path, bytes).map_err(|source| HybridError::Io {
        path: path.to_path_buf(),
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_payload_is_recorded_by_file_name_so_a_bundle_can_move() {
        assert_eq!(file_name(Path::new("/tmp/build/demo.kbc")), "demo.kbc");
    }
}
