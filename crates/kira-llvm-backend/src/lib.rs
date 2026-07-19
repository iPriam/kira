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
mod exports;
// Linking only exists where codegen does: a build without LLVM produces no
// object to link, so the driver would be unreachable rather than merely unused.
#[cfg(feature = "llvm")]
mod link;

pub use exports::{NativeClass, NativeExport, NativeExportSurface};
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
    /// A `break`/`continue` reached codegen with no enclosing loop, which
    /// analysis is supposed to have rejected.
    #[error(
        "a `break`/`continue` reached the LLVM backend outside a loop (this is a compiler bug)"
    )]
    JumpOutsideLoop,
    /// No usable LLVM installation was found.
    #[error(transparent)]
    Discovery(#[from] LlvmDiscoveryError),
    /// The program uses something the native backend cannot lower yet.
    #[error("the LLVM backend cannot lower {0} yet")]
    Unsupported(&'static str),
    /// A struct reached the `@Native`/`@Runtime` boundary, which has no layout
    /// for one.
    ///
    /// `BridgeValue` is a tag plus a one-word payload: a struct neither fits it
    /// nor has a tag, and passing one would need an ABI decision — by value or
    /// by pointer, and who frees the strings inside — that has not been made.
    /// Native code and VM code each handle structs perfectly well; only the
    /// crossing between them is unbuilt.
    #[error(
        "a struct cannot cross the `@Native`/`@Runtime` boundary yet; \
         pass its fields individually, or keep both sides on one engine"
    )]
    StructAtSeam,
    /// An array reached the `@Native`/`@Runtime` seam, which cannot carry one
    /// yet.
    ///
    /// A gap rather than a decision, and that is the difference from
    /// [`LlvmError::StructAtSeam`]: the language *does* let an array cross. It
    /// does not here because the ownership question at the boundary is
    /// unanswered — who frees the elements, and what it means for the VM's heap
    /// accounting if a native callee grows the array it was handed. A wrong
    /// answer is a double free or a leak at the boundary, so the crossing is
    /// refused until the answer is designed.
    #[error(
        "an array cannot cross the `@Native`/`@Runtime` boundary yet; \
         keep both sides on one engine, or pass its elements individually"
    )]
    ArrayAtSeam,
    /// An enum reached the `@Native`/`@Runtime` seam, which has no layout for
    /// one — like a struct, it is a tagged value that does not fit one tag and
    /// one word, and how it would cross is a language decision nobody has made.
    #[error(
        "an enum cannot cross the `@Native`/`@Runtime` boundary; \
         keep both sides on one engine"
    )]
    EnumAtSeam,
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
    /// Where the static archive is written, for a library build.
    ///
    /// A Rust consumer links the archive rather than the dylib: it needs no
    /// deployment story — no question of where the library lives at load time or
    /// how it is found — because the code ends up inside the consumer's own
    /// binary.
    pub archive_path: Option<PathBuf>,
    /// What this library exports, for a library build.
    ///
    /// Empty for a program and for a hybrid half, which are entered another way
    /// entirely.
    pub exports: NativeExportSurface,
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
    /// The linked static archive, for a library build.
    ///
    /// Self-contained: the runtime archive's members are inside it, so a
    /// consumer links one file and needs no arrangement with the Kira
    /// toolchain.
    pub archive: Option<PathBuf>,
    /// The linked shared library, for a library build.
    ///
    /// Exclusive with [`NativeArtifacts::executable`] in practice: a program
    /// produces one and a library the other, which is what makes "this build
    /// produced a library" checkable rather than asserted.
    pub library: Option<PathBuf>,
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
        // A program build produces an executable, never a library; the two are
        // exclusive, which is what makes "this build produced a library"
        // checkable.
        archive: None,
        library: None,
        ir: options.ir_path.clone(),
    })
}

/// Compiles a Kira library to a native object and links it for a consumer.
///
/// The artifact is a library, not an executable: no C `main` is emitted, so
/// there is nothing for an operating system to start. What a consumer reaches
/// it through instead is [`NativeBuildOptions::exports`] — one stable
/// trampoline per export, one synthesized destructor per exported class, and
/// the per-library ABI marker that makes a stale build fail the consumer's
/// link by name.
///
/// Two forms come out, and a Rust consumer wants the first:
///
/// - a **static archive** (`lib<name>.a`) carrying this library's code *and*
///   the runtime archive's members, so the consumer links one file;
/// - a **shared library** (`lib<name>.dylib`/`.so`), for a host that would
///   rather `dlopen` than link.
///
/// Both are self-contained in the same way a hybrid native half is.
#[cfg(feature = "llvm")]
pub fn build_native_library(
    program: &IrProgram,
    options: &NativeBuildOptions,
) -> Result<NativeArtifacts, LlvmError> {
    let module = codegen::Module::build_library(program, &options.module_name, &options.exports)?;
    if let Some(path) = &options.ir_path {
        module.write_ir(path)?;
    }
    module.emit_object(&options.object_path)?;

    if options.archive_path.is_none() && options.shared_library_path.is_none() {
        return Err(LlvmError::Unsupported(
            "a library build with nowhere to put the library",
        ));
    }
    let llvm = kira_toolchain::discover(None)?;
    if let Some(archive) = &options.archive_path {
        link::archive_static_library(
            &llvm,
            &options.object_path,
            &options.runtime_archive,
            archive,
        )?;
    }
    if let Some(library) = &options.shared_library_path {
        link::link_shared_library(
            &llvm,
            &options.object_path,
            &options.runtime_archive,
            library,
        )?;
    }

    Ok(NativeArtifacts {
        object: options.object_path.clone(),
        // A library has no executable by construction; the artifacts are the
        // archive and the shared library.
        executable: None,
        archive: options.archive_path.clone(),
        library: options.shared_library_path.clone(),
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

/// Compiles a Kira library to a native shared library — unavailable in this
/// build.
#[cfg(not(feature = "llvm"))]
pub fn build_native_library(
    _program: &IrProgram,
    _options: &NativeBuildOptions,
) -> Result<NativeArtifacts, LlvmError> {
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
