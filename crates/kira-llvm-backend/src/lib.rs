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

use std::path::{Path, PathBuf};

use kira_debug::DebugInfo;
use kira_ir::IrProgram;
use kira_runtime_abi::ForeignSignature;
use kira_toolchain::LlvmDiscoveryError;

// Re-exported because it is the type of a public option field: a caller
// building a `NativeBuildOptions` must be able to name it without taking a
// dependency of its own on the model crate.
pub use kira_native_lib_definition::NativeLinkInputs;

mod codegen;
mod exports;
#[cfg(test)]
mod foreign_integration_tests;
mod link;
// Not gated: the platform link list is data about a host rather than something
// LLVM answers, and a consumer's build script reads it on a machine with none.
mod platform;
mod reachability;
pub mod shim;
#[cfg(test)]
mod shim_tests;

pub use exports::{NativeClass, NativeExport, NativeExportSurface};
pub use link::{LinkError, link_ffi_carrier};
pub use platform::{PLATFORM_LINK_LISTS, PlatformLinkList, host_link_list, link_list_for};

/// The exported symbol of the generated adapter for foreign import `index`.
///
/// A wire contract shared by every backend and every host: the LLVM backend
/// defines this symbol, the VM sidecar host resolves it, and the hybrid manifest
/// records it. The spelling lives in the runtime ABI crate.
pub fn adapter_name(index: usize) -> String {
    kira_runtime_abi::foreign_adapter_name(index)
}

/// The exported symbol C holds for callback `index`.
///
/// The other half of the same wire contract [`adapter_name`] carries: the
/// backend defines this symbol, and the VM's host resolves it by name to get the
/// address a `@FFI.Callback` value holds.
///
/// *Which* half of the build defines it depends on the signature. A scalar-only
/// callback is entered directly, so LLVM emits this symbol itself. One whose
/// signature takes a struct by value cannot be: only a C compiler knows how that
/// struct arrives, so the generated shim defines this symbol with the true C
/// prototype and forwards to [`callback_body_name`]. Either way the address C
/// holds is this name, which is why no host has to know the difference.
pub fn callback_name(index: usize) -> String {
    kira_runtime_abi::foreign_callback_name(index)
}

/// The symbol LLVM's entry thunk for callback `index` is defined under when the
/// generated shim owns [`callback_name`].
///
/// Never the address C holds: the shim's entry is, and this is what it calls
/// with each by-value struct replaced by its address.
pub fn callback_body_name(index: usize) -> String {
    format!("kira_ffi_callback_body_{index}")
}

/// The symbol LLVM defines the entry thunk for callback `index` under.
///
/// [`callback_name`] for a signature LLVM can present to C on its own, and
/// [`callback_body_name`] for one whose by-value struct the shim classifies.
pub fn callback_thunk_symbol(index: usize, signature: &ForeignSignature) -> String {
    let _ = signature;
    callback_name(index)
}

/// What went wrong producing native code.
#[derive(Debug, thiserror::Error)]
pub enum LlvmError {
    /// A frontend invariant was violated before LLVM lowering.
    #[error("{what} reached the LLVM backend (this is a compiler bug)")]
    Internal {
        /// What was found, named as the invariant that did not hold.
        what: String,
    },
    /// A `break`/`continue` reached codegen with no enclosing loop, which
    /// analysis is supposed to have rejected.
    #[error(
        "a `break`/`continue` reached the LLVM backend outside a loop (this is a compiler bug)"
    )]
    JumpOutsideLoop,
    /// A read through an `@FFI.Pointer` named a member the target's C layout
    /// does not describe as a loadable scalar, which analysis is supposed to
    /// have rejected.
    #[error("a read of C-layout member {member} reached the LLVM backend (this is a compiler bug)")]
    ForeignMemberMissing {
        /// The member index the read asked for.
        member: u32,
    },
    /// A native library build did not name an archive or shared-library output.
    #[error("a native library build needs an archive path or a shared-library path")]
    MissingLibraryOutput,
    /// A hybrid native half did not name its shared-library output.
    #[error("a hybrid native build needs a shared-library path")]
    MissingHybridLibraryPath,
    /// A whole-program native live build did not name its shared-library output.
    #[error("a native live build needs a shared-library path")]
    MissingNativeLiveLibraryPath,
    /// No usable LLVM installation was found.
    #[error(transparent)]
    Discovery(#[from] LlvmDiscoveryError),
    /// The native FFI runtime could not be bundled into a native artifact.
    #[error(transparent)]
    FfiRuntime(#[from] kira_libffi::LibffiError),
    /// This compiler was built against a managed LLVM carrying no WebAssembly
    /// code generator, so it can emit for every device except the Web.
    #[error(
        "this compiler was built against a managed LLVM without the WebAssembly \
         code generator, so it cannot emit for the Web; install a bundle built \
         with the targets `llvm-metadata.toml` pins (`knvm install-llvm --force`) \
         and rebuild the compiler against it"
    )]
    WasmTargetMissing,
    /// The managed clang refused the generated C shim — always a backend bug,
    /// since Kira wrote every line of it.
    ///
    /// The source path is named rather than the text inlined: the file is left
    /// on disk beside the object, so the diagnostic points at something that can
    /// be read and compiled by hand.
    #[error("the managed clang refused the generated foreign shim `{source_path}`:\n{stderr}", source_path = source_path.display())]
    ShimUncompilable {
        /// The generated C file, left in place for inspection.
        source_path: PathBuf,
        /// The compiler's diagnostics.
        stderr: String,
    },
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

impl LlvmError {
    /// An invariant the frontend proves, found not to hold.
    #[must_use]
    pub fn internal(what: impl Into<String>) -> Self {
        LlvmError::Internal { what: what.into() }
    }
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
    /// Where the shared library is written, for a hybrid or native live build.
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
    /// The resolved C link inputs that satisfy the program's `@FFI.Extern`
    /// imports: archives in link order plus the frameworks, system libraries,
    /// and linker flags declared beside them. Empty for a program with no
    /// foreign imports.
    pub foreign_link: NativeLinkInputs,
    /// Whether to optimize the emitted code.
    ///
    /// Optimizing a large module is the dominant cost of a native build — two
    /// minutes against seconds for the editor — so a development build leaves
    /// it off and a shipped one turns it on.
    pub optimize: bool,
    /// The imports whose library is absent on this target, by index.
    ///
    /// Their adapters return
    /// [`ForeignAdapterStatus::UNAVAILABLE_LIBRARY`](kira_runtime_abi::ForeignAdapterStatus::UNAVAILABLE_LIBRARY)
    /// without naming the C symbol, so a Direct3D binding compiled on macOS
    /// contributes no undefined reference to the link.
    pub unavailable_imports: Vec<usize>,
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
    /// The linked shared library, for a library or native live build.
    ///
    /// Exclusive with [`NativeArtifacts::executable`] in practice: a program
    /// produces one and a library the other, which is what makes "this build
    /// produced a library" checkable rather than asserted.
    pub library: Option<PathBuf>,
    /// The textual LLVM IR dump, when one was requested.
    pub ir: Option<PathBuf>,
}

/// The smallest number of functions worth giving a codegen unit of its own.
///
/// Every unit re-declares the program's types and runtime and re-emits the
/// internal leaves it happens to need, and every unit is one more object on the
/// link line. Below this the split costs more than the thread saves.
const FUNCTIONS_PER_UNIT: usize = 96;

/// The stack a codegen worker runs on.
///
/// Lowering walks a program's types structurally — a struct's fields, an
/// array's element, an enum's payload — and a widget tree is as deep as the
/// application that declares it. The main thread is given a large stack by the
/// workspace's own build settings; a spawned thread is given the platform
/// default, which is 2 MiB and is not enough for a real user interface. A
/// compiler that could analyze a program and then overflow emitting it is one
/// that fails after saying it succeeded.
const CODEGEN_STACK: usize = 64 * 1024 * 1024;

/// The most codegen units one program is split into.
///
/// A ceiling on the duplicated scaffold and the link line, not on the machine:
/// the units are also capped by the parallelism available, and a host with more
/// cores than this has stopped being the bottleneck.
const MAX_CODEGEN_UNITS: usize = 16;

/// How many codegen units this program is emitted in.
///
/// One unit for a program small enough that the split would not pay for itself,
/// and one whenever a textual IR dump was asked for — `--emit-llvm-ir` is a
/// request to read the program's module, and handing back eight of them
/// answers a different question.
fn codegen_units(options: &NativeBuildOptions, reachable: &[bool]) -> usize {
    if options.ir_path.is_some() {
        return 1;
    }
    let parallelism = std::thread::available_parallelism().map_or(1, std::num::NonZero::get);
    let function_count = reachable.iter().filter(|reachable| **reachable).count();
    let affordable = (function_count / FUNCTIONS_PER_UNIT).min(MAX_CODEGEN_UNITS);
    affordable.clamp(1, parallelism.max(1))
}

/// Where unit `index`'s object is written, given the build's object path.
///
/// The first unit keeps the path the build named, because that path is what
/// names the foreign shim beside it and what a caller reports as *the* object.
fn unit_object_path(object_path: &Path, index: usize) -> PathBuf {
    if index == 0 {
        return object_path.to_path_buf();
    }
    let extension = object_path.extension().map_or_else(
        || "o".to_owned(),
        |extension| extension.to_string_lossy().into_owned(),
    );
    object_path.with_extension(format!("{index}.{extension}"))
}

#[derive(Clone, Copy)]
enum NativeModuleKind {
    Executable,
    LiveLibrary,
}

fn build_codegen_module(
    program: &IrProgram,
    module_name: &str,
    unavailable: &[usize],
    unit: codegen::CodegenUnit,
    debug: Option<&DebugInfo>,
    kind: NativeModuleKind,
) -> Result<codegen::Module, LlvmError> {
    match kind {
        NativeModuleKind::Executable => match debug {
            Some(debug) => codegen::Module::build_debug(
                program,
                module_name,
                kira_runtime_abi::ForeignPointerWidth::HOST,
                unavailable,
                unit,
                debug,
            ),
            None => codegen::Module::build(
                program,
                module_name,
                kira_runtime_abi::ForeignPointerWidth::HOST,
                unavailable,
                unit,
            ),
        },
        NativeModuleKind::LiveLibrary => {
            codegen::Module::build_native_live(program, module_name, unavailable, unit)
        }
    }
}

/// Adds the Rust helper archive and bundled DLL used by native libffi calls to
/// the same link/staging set every native backend consumes.
fn ffi_link_inputs(
    program: &IrProgram,
    foreign_link: &NativeLinkInputs,
    unavailable: &[usize],
) -> Result<NativeLinkInputs, LlvmError> {
    let has_callable_foreign = program
        .foreign_imports
        .iter()
        .enumerate()
        .any(|(index, _)| !unavailable.contains(&index));
    if !has_callable_foreign && program.foreign_callbacks.is_empty() {
        return Ok(foreign_link.clone());
    }
    let mut link = foreign_link.clone();
    link.push_archive(kira_libffi::runtime_archive()?);
    link.push_runtime_file(kira_libffi::bundled_path()?);
    Ok(link)
}

/// Lowers and emits the program's objects, one per codegen unit, in parallel.
///
/// Returns them in unit order, first unit first.
fn emit_codegen_units(
    program: &IrProgram,
    options: &NativeBuildOptions,
    debug: Option<&DebugInfo>,
    kind: NativeModuleKind,
) -> Result<Vec<PathBuf>, LlvmError> {
    let reachable = reachability::native_functions(program);
    let count = codegen_units(options, &reachable);
    let paths: Vec<PathBuf> = (0..count)
        .map(|index| unit_object_path(&options.object_path, index))
        .collect();

    if count == 1 {
        kira_diagnostics::progress!("generating native code for {}", options.module_name);
        let module = build_codegen_module(
            program,
            &options.module_name,
            &options.unavailable_imports,
            codegen::CodegenUnit::WHOLE,
            debug,
            kind,
        )?;
        if let Some(path) = &options.ir_path {
            module.write_ir(path)?;
        }
        module.emit_object(&options.object_path, options.optimize)?;
        return Ok(paths);
    }

    kira_diagnostics::progress!(
        "generating native code for {} in {count} units",
        options.module_name
    );
    std::thread::scope(|scope| {
        let workers: Vec<_> = paths
            .iter()
            .enumerate()
            .map(|(index, path)| {
                let worker = std::thread::Builder::new()
                    .name(format!("kira-codegen-{index}"))
                    .stack_size(CODEGEN_STACK);
                worker.spawn_scoped(scope, move || {
                    let module = build_codegen_module(
                        program,
                        &format!("{}.{index}", options.module_name),
                        &options.unavailable_imports,
                        codegen::CodegenUnit::new(index, count),
                        debug,
                        kind,
                    )?;
                    module.emit_object(path, options.optimize)
                })
            })
            .collect::<Result<Vec<_>, _>>()
            .map_err(|source| LlvmError::Emit(format!("cannot start a codegen unit: {source}")))?;
        for worker in workers {
            // A worker that panicked took its LLVM context down with it, and
            // the module it was building is gone; there is nothing to report
            // but that the emission did not finish.
            worker.join().map_err(|_| {
                LlvmError::Emit("a codegen unit failed while emitting its object".to_owned())
            })??;
        }
        Ok::<(), LlvmError>(())
    })?;
    Ok(paths)
}

/// Compiles `program` to a native object, and links an executable when
/// [`NativeBuildOptions::executable_path`] asks for one.
pub fn build_native(
    program: &IrProgram,
    options: &NativeBuildOptions,
) -> Result<NativeArtifacts, LlvmError> {
    build_native_inner(program, options, None)
}

/// Compiles and links a native executable with DWARF and native debug data.
pub fn build_native_debug(
    program: &IrProgram,
    options: &NativeBuildOptions,
    debug: &DebugInfo,
) -> Result<NativeArtifacts, LlvmError> {
    build_native_inner(program, options, Some(debug))
}

fn build_native_inner(
    program: &IrProgram,
    options: &NativeBuildOptions,
    debug: Option<&DebugInfo>,
) -> Result<NativeArtifacts, LlvmError> {
    let objects = emit_codegen_units(program, options, debug, NativeModuleKind::Executable)?;
    let foreign_link =
        ffi_link_inputs(program, &options.foreign_link, &options.unavailable_imports)?;

    let executable = match &options.executable_path {
        Some(path) => {
            let llvm = kira_toolchain::discover(None)?;
            kira_diagnostics::progress!("linking {}", path.display());
            match debug {
                Some(debug) => link::link_executable_debug(
                    &llvm,
                    &objects,
                    &options.runtime_archive,
                    &foreign_link,
                    path,
                    &debug_symbols(program, debug, true),
                )?,
                None => link::link_executable(
                    &llvm,
                    &objects,
                    &options.runtime_archive,
                    &foreign_link,
                    path,
                )?,
            }
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

/// Compiles a whole program into the shared library used by an LLVM live
/// session.
///
/// Unlike a hybrid library, this emits every reachable function as native code
/// and a single fixed runner entry symbol. Foreign adapters and their C shim are
/// linked into the same artifact, so the runner does not need a second native
/// surface for the program to be complete.
pub fn build_native_live(
    program: &IrProgram,
    options: &NativeBuildOptions,
) -> Result<NativeArtifacts, LlvmError> {
    let library = options
        .shared_library_path
        .clone()
        .ok_or(LlvmError::MissingNativeLiveLibraryPath)?;
    let objects = emit_codegen_units(program, options, None, NativeModuleKind::LiveLibrary)?;
    let foreign_link =
        ffi_link_inputs(program, &options.foreign_link, &options.unavailable_imports)?;
    let llvm = kira_toolchain::discover(None)?;
    kira_diagnostics::progress!("linking {}", library.display());
    link::link_native_live_library(
        &llvm,
        &objects,
        &options.runtime_archive,
        &foreign_link,
        &library,
    )?;

    Ok(NativeArtifacts {
        object: options.object_path.clone(),
        executable: None,
        archive: None,
        library: Some(library),
        ir: options.ir_path.clone(),
    })
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
    object_path: &Path,
    device: kira_backend_api::WasmDevice,
) -> Result<(), LlvmError> {
    // A wasm module lays out pointers at the selected device width, and the
    // aggregate offsets are computed during lowering.
    let width = match device {
        kira_backend_api::WasmDevice::Wasm32 => kira_runtime_abi::ForeignPointerWidth::Bits32,
        kira_backend_api::WasmDevice::Wasm64 => kira_runtime_abi::ForeignPointerWidth::Bits64,
    };
    let module = codegen::Module::build_wasm(
        program,
        module_name,
        width,
        &[],
        codegen::CodegenUnit::WHOLE,
        device,
    )?;
    module.emit_wasm_object(object_path, device)
}

/// Compiles a Kira library to a WebAssembly object for `device`.
///
/// The object has no `main`. Its entry surface is the same uniform
/// `kira_lib_*` trampoline set used by native library consumers, with the
/// target's pointer width applied while lowering rather than after the module
/// has already been laid out.
pub fn build_wasm_library(
    program: &IrProgram,
    module_name: &str,
    object_path: &Path,
    device: kira_backend_api::WasmDevice,
    exports: &NativeExportSurface,
) -> Result<(), LlvmError> {
    let module = codegen::Module::build_wasm_library(program, module_name, exports, device)?;
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
    if options.archive_path.is_none() && options.shared_library_path.is_none() {
        return Err(LlvmError::MissingLibraryOutput);
    }
    let module = codegen::Module::build_library(program, &options.module_name, &options.exports)?;
    if let Some(path) = &options.ir_path {
        module.write_ir(path)?;
    }
    module.emit_object(&options.object_path, options.optimize)?;

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
    build_hybrid_library_inner(program, options, None)
}

/// Builds a hybrid native half with DWARF and native debug data.
pub fn build_hybrid_library_debug(
    program: &IrProgram,
    options: &NativeBuildOptions,
    debug: &DebugInfo,
) -> Result<HybridArtifacts, LlvmError> {
    build_hybrid_library_inner(program, options, Some(debug))
}

/// Reports whether an application hybrid build has a reachable native body.
///
/// A native function that no entrypoint or callback can reach is omitted from
/// the native half and does not prevent the CLI from reusing that half.
#[must_use]
pub fn has_reachable_hybrid_native_functions(program: &IrProgram) -> bool {
    let reachable = reachability::hybrid_native_functions(program);
    program
        .functions
        .iter()
        .enumerate()
        .any(|(index, function)| {
            function
                .execution
                .resolve(kira_runtime_abi::Execution::Runtime)
                == kira_runtime_abi::Execution::Native
                && reachable.get(index).copied().unwrap_or(false)
        })
}

/// Reports whether a reachable native body uses the compiler runtime.
///
/// Compiler expressions in runtime-only bodies stay in the VM and do not
/// require the larger compiler bridge archive in an application hybrid half.
#[must_use]
pub fn hybrid_uses_compiler_runtime(program: &IrProgram) -> bool {
    reachability::hybrid_uses_compiler(program)
}

fn build_hybrid_library_inner(
    program: &IrProgram,
    options: &NativeBuildOptions,
    debug: Option<&DebugInfo>,
) -> Result<HybridArtifacts, LlvmError> {
    let library = options
        .shared_library_path
        .clone()
        .ok_or(LlvmError::MissingHybridLibraryPath)?;
    let module = match debug {
        Some(debug) => codegen::Module::build_hybrid_debug(
            program,
            &options.module_name,
            &options.unavailable_imports,
            debug,
        )?,
        None => codegen::Module::build_hybrid(
            program,
            &options.module_name,
            &options.unavailable_imports,
        )?,
    };
    if let Some(path) = &options.ir_path {
        module.write_ir(path)?;
    }
    module.emit_object(&options.object_path, options.optimize)?;

    let llvm = kira_toolchain::discover(None)?;
    let foreign_link =
        ffi_link_inputs(program, &options.foreign_link, &options.unavailable_imports)?;
    let static_names: std::collections::HashSet<&str> = foreign_link
        .static_archives()
        .iter()
        .map(|(name, _)| name.as_str())
        .collect();
    let mut retained_symbols: Vec<String> = (0..program.foreign_callbacks.len())
        .map(callback_name)
        .collect();
    retained_symbols.extend(
        program
            .foreign_imports
            .iter()
            .filter(|entry| static_names.contains(entry.import.library()))
            .map(|entry| entry.import.symbol().to_owned()),
    );
    retained_symbols.sort();
    retained_symbols.dedup();
    match debug {
        Some(debug) => link::link_hybrid_library_debug(
            &llvm,
            &options.object_path,
            &options.runtime_archive,
            &foreign_link,
            &retained_symbols,
            &library,
            &debug_symbols(program, debug, false),
        )?,
        None => link::link_hybrid_library(
            &llvm,
            &options.object_path,
            &options.runtime_archive,
            &foreign_link,
            &retained_symbols,
            &library,
        )?,
    }

    Ok(HybridArtifacts {
        object: options.object_path.clone(),
        library,
        exports: exported_trampolines(program),
    })
}

/// Selects exactly the body symbols emitted by a debug native module.
fn debug_symbols(program: &IrProgram, info: &DebugInfo, executable: bool) -> Vec<String> {
    let reachable = executable.then(|| reachability::native_functions(program));
    program
        .functions
        .iter()
        .enumerate()
        .filter(|(index, function)| {
            let native = executable
                || function
                    .execution
                    .resolve(kira_runtime_abi::Execution::Runtime)
                    == kira_runtime_abi::Execution::Native;
            native
                && reachable
                    .as_ref()
                    .is_none_or(|reachable| reachable.get(*index).copied().unwrap_or(false))
        })
        .filter_map(|(index, _)| info.functions.get(index)?.symbol.clone())
        .collect()
}

/// The trampoline symbol exported for each `@Native` function, by function id.
fn exported_trampolines(program: &IrProgram) -> Vec<(u32, String)> {
    let reachable = reachability::hybrid_native_functions(program);
    program
        .functions
        .iter()
        .enumerate()
        .filter(|(index, function)| {
            function
                .execution
                .resolve(kira_runtime_abi::Execution::Runtime)
                == kira_runtime_abi::Execution::Native
                && reachable.get(*index).copied().unwrap_or(false)
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
