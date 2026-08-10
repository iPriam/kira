//! The hybrid half of `build` and `run`: emitting a bundle, and running it.
//!
//! A hybrid build writes three artifacts into `.kira-build/`, from one IR:
//!
//! - `<stem>.kbc` — the bytecode half, every non-`@Native` function,
//! - `lib<stem>.dylib` (or `.so`) — the native half, one trampoline per
//!   `@Native` function,
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

use kira_hybrid_definition::HybridManifest;
use kira_ir::IrProgram;
use kira_llvm_backend::{NativeBuildOptions, NativeLinkInputs};
use kira_runtime_abi::Execution;

use crate::native::{self, Artifacts, NativeError};

/// The artifacts a hybrid build produced.
pub struct HybridBundle {
    /// The manifest a run loads first.
    pub manifest: PathBuf,
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
    foreign_link: &NativeLinkInputs,
) -> Result<HybridBundle, HybridError> {
    let artifacts =
        Artifacts::for_source(source).map_err(|source| NativeError::Layout { source })?;

    // The bytecode half: every function that is not `@Native`.
    let module = kira_bytecode::compile_hybrid(program)?;
    let bytecode_path = artifacts.bytecode();
    write(&bytecode_path, &module.to_bytes())?;

    // A VM live bundle carries a native library only for foreign adapters and
    // callbacks. Its Kira function bodies are all in the bytecode half, so a
    // source edit that leaves this crossing surface unchanged must not invoke
    // LLVM and the linker again.
    let reusable_native = program
        .functions
        .iter()
        .all(|function| function.execution.resolve(Execution::Runtime) == Execution::Runtime);
    let surface_key = native_surface_key(program, foreign_link);
    let cache_path = artifacts.native_surface_key();
    let cache_hit = reusable_native
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
            runtime_archive: native::runtime_archive(program)?,
            optimize: false,
            unavailable_imports: foreign_link.unavailable_imports().to_vec(),
            foreign_link: foreign_link.clone(),
        };
        let native = kira_llvm_backend::build_hybrid_library(program, &options)?;
        if reusable_native {
            std::fs::write(&cache_path, &surface_key).map_err(|source| HybridError::Io {
                path: cache_path.clone(),
                source,
            })?;
        }
        native.exports
    };

    // The manifest, last: it names the two payloads above.
    let manifest = manifest(
        program,
        &artifacts,
        &exports,
        kira_build::hybrid_internal_function_count(program, &module)?,
    )?;
    let manifest_path = artifacts.manifest();
    write(&manifest_path, &manifest.to_bytes())?;

    Ok(HybridBundle {
        manifest: manifest_path,
    })
}

/// The exact input that can change a VM live session's native adapter library.
///
/// The version makes an old compiler's `.kira-build` output conservative after
/// a native ABI change. The ordinary Kira function bodies are deliberately not
/// present: they do not enter the native half of a VM bundle.
fn native_surface_key(program: &IrProgram, foreign_link: &NativeLinkInputs) -> String {
    format!(
        "kira-vm-native-surface-v1\nimports={:?}\naggregates={:?}\ncallbacks={:?}\nlink={:?}",
        program.foreign_imports,
        program.foreign_aggregates,
        program.foreign_callbacks,
        foreign_link,
    )
}

/// Builds a hybrid bundle and runs it, returning the program's exit code.
pub fn run(
    program: &IrProgram,
    source: &Path,
    emit_llvm_ir: bool,
    foreign_link: &NativeLinkInputs,
) -> Result<i32, HybridError> {
    let bundle = build(program, source, emit_llvm_ir, foreign_link)?;
    let session = kira_hybrid_runtime::Session::load(&bundle.manifest)?;
    session.run()?;
    Ok(0)
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
) -> Result<HybridManifest, HybridError> {
    Ok(kira_build::hybrid_manifest(
        program,
        artifacts.stem(),
        &file_name(&artifacts.bytecode()),
        &file_name(&artifacts.shared_library()),
        exports,
        internal_functions,
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
