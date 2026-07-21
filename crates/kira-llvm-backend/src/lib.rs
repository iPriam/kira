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
//! - LLVM is a hard dependency: every build of this crate carries the backend,
//!   and a machine without a managed LLVM does not build the workspace. The
//!   `verifying-work` provisioning notes name the fix.
//! - `unsafe` is fenced to this crate's binding layer, with a `// SAFETY:`
//!   comment on every block.

use std::path::PathBuf;

use kira_ir::IrProgram;
use kira_toolchain::LlvmDiscoveryError;

mod codegen;
mod exports;
#[cfg(test)]
mod foreign_integration_tests;
mod link;
// Not gated: the platform link list is data about a host rather than something
// LLVM answers, and a consumer's build script reads it on a machine with none.
mod platform;

pub use exports::{NativeClass, NativeExport, NativeExportSurface};
pub use link::LinkError;
pub use platform::{PLATFORM_LINK_LISTS, PlatformLinkList, host_link_list, link_list_for};

/// The exported symbol of the generated adapter for foreign import `index`.
///
/// A wire contract shared by every backend and every host: the LLVM backend
/// defines this symbol, the VM sidecar host resolves it, and the hybrid manifest
/// records it. Spelled once here so a producer and a consumer cannot disagree.
pub fn adapter_name(index: usize) -> String {
    format!("kira_foreign_adapter_{index}")
}

/// What went wrong producing native code.
#[derive(Debug, thiserror::Error)]
pub enum LlvmError {
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
    /// The managed LLVM was built without the WebAssembly code generator.
    #[error(
        "the managed LLVM has no WebAssembly code generator; re-provision the \
         bundle (`llvm-metadata.toml` now pins `host;WebAssembly` targets)"
    )]
    WasmTargetMissing,
    /// Lowering produced a module LLVM rejected — always a backend bug.
    #[error("LLVM rejected the generated module (this is a compiler bug): {0}")]
    InvalidModule(String),
    /// LLVM could not emit an object for this target.
    #[error("LLVM could not emit an object file: {0}")]
    Emit(String),
    /// Linking the native executable failed.
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
    /// The selected C static archives that satisfy the program's `@FFI.Extern`
    /// imports, in link order. Empty for a program with no foreign imports.
    pub foreign_archives: Vec<PathBuf>,
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
            link::link_executable(
                &llvm,
                &options.object_path,
                &options.runtime_archive,
                &options.foreign_archives,
                path,
            )?;
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

/// What the VM's foreign-adapter sidecar build should produce, and where.
#[derive(Debug, Clone, PartialEq)]
pub struct AdapterSidecarOptions {
    /// The module name recorded in the emitted artifacts.
    pub module_name: String,
    /// Where the adapters-only object file is written.
    pub object_path: PathBuf,
    /// Where the linked sidecar shared library is written.
    pub library_path: PathBuf,
    /// The native runtime archive (`libkira_native_bridge.a`) to link against.
    pub runtime_archive: PathBuf,
    /// The selected C static archives that satisfy the program's foreign
    /// imports, in link order.
    pub foreign_archives: Vec<PathBuf>,
}

/// Compiles the program's foreign adapters into one loadable sidecar library.
///
/// The VM never links or dlopens anything itself; this is what the CLI build
/// produces so a native-capable host can answer `call_foreign`. The sidecar
/// carries one exported adapter per foreign import, the foreign-adapter marker,
/// the string helpers the loader binds, and the selected C archives — all
/// self-contained, so the host loads one file. Returns the sidecar path.
pub fn build_adapter_sidecar(
    program: &IrProgram,
    options: &AdapterSidecarOptions,
) -> Result<PathBuf, LlvmError> {
    let module = codegen::Module::build_adapter_sidecar(program, &options.module_name)?;
    module.emit_object(&options.object_path)?;
    let llvm = kira_toolchain::discover(None)?;
    let adapter_symbols: Vec<String> = (0..program.foreign_imports.len())
        .map(adapter_name)
        .collect();
    link::link_adapter_sidecar(
        &llvm,
        &options.object_path,
        &options.runtime_archive,
        &options.foreign_archives,
        &adapter_symbols,
        &options.library_path,
    )?;
    Ok(options.library_path.clone())
}

/// Compiles `program` to a WebAssembly object for `device`.
///
/// Codegen is the same in-process C-API path as the host's — the lowering is
/// shared, only the target machine differs — and linking is not done here: the
/// caller drives the Web linker (emscripten) over the object exactly as the
/// host path drives `clang`, so no textual IR exists anywhere in either.
pub fn build_wasm_object(
    program: &IrProgram,
    module_name: &str,
    object_path: &std::path::Path,
    device: kira_backend_api::WasmDevice,
) -> Result<(), LlvmError> {
    let module = codegen::Module::build(program, module_name)?;
    module.emit_wasm_object(object_path, device)
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
    // One adapter per foreign import lives in this same dylib; force each in and
    // link the C archives that satisfy them, so the hybrid session binds every
    // foreign call out of the one native half.
    let adapter_symbols: Vec<String> = (0..program.foreign_imports.len())
        .map(adapter_name)
        .collect();
    link::link_hybrid_library(
        &llvm,
        &options.object_path,
        &options.runtime_archive,
        &options.foreign_archives,
        &adapter_symbols,
        &library,
    )?;

    Ok(HybridArtifacts {
        object: options.object_path.clone(),
        library,
        exports: exported_trampolines(program),
    })
}

/// The trampoline symbol exported for each `@Native` function, by function id.
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The adapter symbol is a wire contract shared with the VM host and the
    /// hybrid manifest, so it is pinned rather than only round-tripped.
    #[test]
    fn adapter_symbols_are_pinned_per_import_index() {
        assert_eq!(adapter_name(0), "kira_foreign_adapter_0");
        assert_eq!(adapter_name(7), "kira_foreign_adapter_7");
    }
}
