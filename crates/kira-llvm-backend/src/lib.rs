//! LLVM/native backend: compiles Kira IR to machine code via LLVM.
//!
//! Layer 4 of the Kira package graph.
//!
//! # Shape
//!
//! The backend consumes the same verified [`IrProgram`] the VM's bytecode
//! compiler does and lowers it *in process* through the LLVM C API
//! (`llvm-sys`), emitting a real object file with LLVM's own target machine —
//! no textual-IR round trip and no `clang -x ir` subprocess in the codegen
//! path. `clang` from the managed toolchain is used only as the linker driver.
//!
//! # Parity
//!
//! Native code must behave exactly as the VM does on the same program, so the
//! lowering mirrors the interpreter's semantics rather than taking the
//! shortcuts a C compiler would:
//!
//! - integer arithmetic wraps (no `nsw`/`nuw`), matching the VM's `wrapping_*`,
//! - `/` and `%` by zero call the runtime's trap helper, and `MIN / -1` is
//!   special-cased to the wrapping result instead of LLVM's poison,
//! - `print` and all string work go through the stable `kira_rt_*` helpers in
//!   `kira-native-bridge`, which format with the same standard library the VM
//!   uses — so output is identical byte-for-byte.
//!
//! # Standing decisions
//!
//! - LLVM is reached through `llvm-sys`, never `inkwell`.
//! - The backend is feature-gated (`llvm`) so the workspace builds and lints on
//!   a machine with no LLVM install; with the feature off the API is unchanged
//!   and reports [`LlvmError::NotCompiledIn`] rather than degrading silently.
//! - `unsafe` is fenced to this crate's binding layer, with a `// SAFETY:`
//!   comment on every block.

use std::path::PathBuf;

use kira_ir::IrProgram;
use kira_toolchain::LlvmDiscoveryError;

#[cfg(feature = "llvm")]
mod codegen;
// Linking only exists where codegen does: a build without LLVM produces no
// object to link, so the driver would be unreachable rather than merely unused.
#[cfg(feature = "llvm")]
mod link;

#[cfg(feature = "llvm")]
pub use link::LinkError;

/// What went wrong producing native code.
#[derive(Debug, thiserror::Error)]
pub enum LlvmError {
    /// The backend was compiled without its `llvm` feature.
    #[error(
        "this kirac was built without the LLVM backend; rebuild with \
         `--features llvm` and a managed LLVM present to use `--backend llvm`"
    )]
    NotCompiledIn,
    /// No usable LLVM installation was found.
    #[error(transparent)]
    Discovery(#[from] LlvmDiscoveryError),
    /// The program uses something the native backend cannot lower yet.
    #[error("the LLVM backend cannot lower {0} yet")]
    Unsupported(&'static str),
    /// Lowering produced a module LLVM rejected — always a backend bug.
    #[error("LLVM rejected the generated module (this is a compiler bug): {0}")]
    InvalidModule(String),
    /// LLVM could not emit an object for this target.
    #[error("LLVM could not emit an object file: {0}")]
    Emit(String),
    /// Linking the native executable failed.
    #[cfg(feature = "llvm")]
    #[error(transparent)]
    Link(#[from] LinkError),
    /// An artifact path could not be written.
    #[error("cannot write `{path}`: {source}")]
    Io {
        /// The path being written.
        path: PathBuf,
        /// The underlying I/O failure.
        #[source]
        source: std::io::Error,
    },
}

/// What a native build should produce, and where.
#[derive(Debug, Clone, PartialEq)]
pub struct NativeBuildOptions {
    /// The module name recorded in the emitted artifacts.
    pub module_name: String,
    /// Where the object file is written.
    pub object_path: PathBuf,
    /// Where the linked executable is written, when one is requested.
    pub executable_path: Option<PathBuf>,
    /// Where the shared library is written, for a hybrid build.
    pub shared_library_path: Option<PathBuf>,
    /// Where the textual LLVM IR is written, when requested. A debugging aid:
    /// it is never an input to the codegen path.
    pub ir_path: Option<PathBuf>,
    /// The native runtime archive (`libkira_native_bridge.a`) to link against.
    pub runtime_archive: PathBuf,
}

/// The artifacts a native build produced.
#[derive(Debug, Clone, PartialEq)]
pub struct NativeArtifacts {
    /// The emitted object file.
    pub object: PathBuf,
    /// The linked executable, when one was requested.
    pub executable: Option<PathBuf>,
    /// The textual LLVM IR dump, when one was requested.
    pub ir: Option<PathBuf>,
}

/// Compiles `program` to a native object, and links an executable when
/// [`NativeBuildOptions::executable_path`] asks for one.
#[cfg(feature = "llvm")]
pub fn build_native(
    program: &IrProgram,
    options: &NativeBuildOptions,
) -> Result<NativeArtifacts, LlvmError> {
    let module = codegen::Module::build(program, &options.module_name)?;
    if let Some(path) = &options.ir_path {
        module.write_ir(path)?;
    }
    module.emit_object(&options.object_path)?;

    let executable = match &options.executable_path {
        Some(path) => {
            let llvm = kira_toolchain::discover(None)?;
            link::link_executable(&llvm, &options.object_path, &options.runtime_archive, path)?;
            Some(path.clone())
        }
        None => None,
    };

    Ok(NativeArtifacts {
        object: options.object_path.clone(),
        executable,
        ir: options.ir_path.clone(),
    })
}

/// Compiles the native half of a hybrid program into a shared library.
///
/// Only `@Native` functions are emitted here, each with a
/// `kira_native_fn_<id>` trampoline the host calls; everything else stays in
/// the bytecode half. Returns the exported trampoline symbol for each native
/// function, which the hybrid manifest records so the host knows what to bind.
#[cfg(feature = "llvm")]
pub fn build_hybrid_library(
    program: &IrProgram,
    options: &NativeBuildOptions,
) -> Result<HybridArtifacts, LlvmError> {
    let module = codegen::Module::build_hybrid(program, &options.module_name)?;
    if let Some(path) = &options.ir_path {
        module.write_ir(path)?;
    }
    module.emit_object(&options.object_path)?;

    let library = options
        .shared_library_path
        .clone()
        .ok_or(LlvmError::Unsupported(
            "a hybrid build with no library path",
        ))?;
    let llvm = kira_toolchain::discover(None)?;
    link::link_shared_library(
        &llvm,
        &options.object_path,
        &options.runtime_archive,
        &library,
    )?;

    Ok(HybridArtifacts {
        object: options.object_path.clone(),
        library,
        exports: exported_trampolines(program),
    })
}

/// The trampoline symbol exported for each `@Native` function, by function id.
#[cfg(feature = "llvm")]
fn exported_trampolines(program: &IrProgram) -> Vec<(u32, String)> {
    program
        .functions
        .iter()
        .enumerate()
        .filter(|(_, function)| {
            function
                .execution
                .resolve(kira_runtime_abi::Execution::Runtime)
                == kira_runtime_abi::Execution::Native
        })
        .map(|(index, _)| (index as u32, codegen::trampoline_name(index)))
        .collect()
}

/// The artifacts a hybrid native build produced.
#[derive(Debug, Clone, PartialEq)]
pub struct HybridArtifacts {
    /// The emitted object file.
    pub object: PathBuf,
    /// The linked shared library holding the native half.
    pub library: PathBuf,
    /// The trampoline symbol exported for each native function, by id.
    pub exports: Vec<(u32, String)>,
}

/// Compiles the native half of a hybrid program — unavailable in this build.
#[cfg(not(feature = "llvm"))]
pub fn build_hybrid_library(
    _program: &IrProgram,
    _options: &NativeBuildOptions,
) -> Result<HybridArtifacts, LlvmError> {
    Err(LlvmError::NotCompiledIn)
}

/// Compiles `program` to a native object — unavailable in this build.
///
/// Without the `llvm` feature the backend reports [`LlvmError::NotCompiledIn`],
/// so a caller fails loudly and actionably instead of silently producing
/// nothing or falling back to another backend.
#[cfg(not(feature = "llvm"))]
pub fn build_native(
    _program: &IrProgram,
    _options: &NativeBuildOptions,
) -> Result<NativeArtifacts, LlvmError> {
    Err(LlvmError::NotCompiledIn)
}

/// Whether this build carries a working LLVM backend.
pub fn is_available() -> bool {
    cfg!(feature = "llvm")
}
