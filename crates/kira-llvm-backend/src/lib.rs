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
mod shim_build;
#[cfg(test)]
mod shim_tests;

pub use exports::{NativeClass, NativeExport, NativeExportSurface};
pub use link::LinkError;
pub use platform::{PLATFORM_LINK_LISTS, PlatformLinkList, host_link_list, link_list_for};
pub use shim_build::ShimObject;

/// The exported symbol of the generated adapter for foreign import `index`.
///
/// A wire contract shared by every backend and every host: the LLVM backend
/// defines this symbol, the VM sidecar host resolves it, and the hybrid manifest
/// records it. Spelled once here so a producer and a consumer cannot disagree.
pub fn adapter_name(index: usize) -> String {
    format!("kira_foreign_adapter_{index}")
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
    format!("kira_ffi_callback_{index}")
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
    if shim::callback_needs_entry(signature) {
        callback_body_name(index)
    } else {
        callback_name(index)
    }
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
    /// A read through an `@FFI.Pointer` named a member the target's C layout
    /// does not describe as a loadable scalar, which analysis is supposed to
    /// have rejected.
    #[error("a read of C-layout member {member} reached the LLVM backend (this is a compiler bug)")]
    ForeignMemberMissing {
        /// The member index the read asked for.
        member: u32,
    },
    /// No usable LLVM installation was found.
    #[error(transparent)]
    Discovery(#[from] LlvmDiscoveryError),
    /// The program uses something the native backend cannot lower yet.
    #[error("the LLVM backend cannot lower {0} yet")]
    Unsupported(&'static str),
    // Struct, array and enum crossings were refused here until the seam grew a
    // way to carry them. A struct and an array now cross as a node tree, and an
    // enum crosses either as its bare variant tag (payload-less) or as a tree
    // (carrying one) — so the three errors that named those refusals are gone
    // rather than left unreachable. The ownership question they each cited has
    // one answer now: the tree is transferred, and the reader frees it as it
    // decodes. See `BridgeValueTag::NODE`.
    //
    // The VM keeps its own `StructAtSeam`/`ArrayAtSeam`/`EnumAtSeam`, which are
    // still reachable: they are the backstop for a *value* with no tree form,
    // which is a different question from a *type* with no crossing.
    /// A value of the top type reached the hybrid seam.
    ///
    /// Not a size problem — an erased value is one word — but a reading one: the
    /// seam's tag tells the far side how to read the payload, and `Any` says
    /// only that some type was erased, which the far side cannot act on.
    #[error(
        "`Any` cannot cross the `@Native`/`@Runtime` boundary; an erased value \
         has no type for the far side to read it back as"
    )]
    AnyAtSeam,
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
    /// The linked shared library, for a library build.
    ///
    /// Exclusive with [`NativeArtifacts::executable`] in practice: a program
    /// produces one and a library the other, which is what makes "this build
    /// produced a library" checkable rather than asserted.
    pub library: Option<PathBuf>,
    /// The textual LLVM IR dump, when one was requested.
    pub ir: Option<PathBuf>,
}

/// Compiles the C shim `program` needs to carry aggregates across the seam, in
/// either direction.
///
/// `None` for a program that neither passes a struct by value nor hands C a
/// callback entered with one, which is every program that has always worked —
/// those never invoke clang for a shim.
fn build_foreign_shim(
    program: &IrProgram,
    unavailable: &[usize],
    object_path: &Path,
    llvm: &kira_toolchain::LlvmInstallation,
) -> Result<Option<ShimObject>, LlvmError> {
    let imports: Vec<_> = program
        .foreign_imports
        .iter()
        .map(|entry| entry.import.clone())
        .collect();
    shim_build::build(
        &imports,
        &program.foreign_callbacks,
        &program.foreign_aggregates,
        unavailable,
        object_path,
        llvm,
    )
}

/// The smallest number of functions worth giving a codegen unit of its own.
///
/// Every unit re-declares the program's types and runtime and re-emits the
/// internal leaves it happens to need, and every unit is one more object on the
/// link line. Below this the split costs more than the thread saves.
const FUNCTIONS_PER_UNIT: usize = 96;

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

/// Lowers and emits the program's objects, one per codegen unit, in parallel.
///
/// Returns them in unit order, first unit first.
fn emit_codegen_units(
    program: &IrProgram,
    options: &NativeBuildOptions,
    debug: Option<&DebugInfo>,
) -> Result<Vec<PathBuf>, LlvmError> {
    let reachable = reachability::native_functions(program);
    let count = codegen_units(options, &reachable);
    let paths: Vec<PathBuf> = (0..count)
        .map(|index| unit_object_path(&options.object_path, index))
        .collect();

    if count == 1 {
        kira_diagnostics::progress!("generating native code for {}", options.module_name);
        let module = match debug {
            Some(debug) => codegen::Module::build_debug(
                program,
                &options.module_name,
                kira_runtime_abi::ForeignPointerWidth::HOST,
                &options.unavailable_imports,
                codegen::CodegenUnit::WHOLE,
                debug,
            )?,
            None => codegen::Module::build(
                program,
                &options.module_name,
                kira_runtime_abi::ForeignPointerWidth::HOST,
                &options.unavailable_imports,
                codegen::CodegenUnit::WHOLE,
            )?,
        };
        if let Some(path) = &options.ir_path {
            module.write_ir(path)?;
        }
        kira_diagnostics::progress!("emitting object");
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
                scope.spawn(move || {
                    let module = match debug {
                        Some(debug) => codegen::Module::build_debug(
                            program,
                            &format!("{}.{index}", options.module_name),
                            kira_runtime_abi::ForeignPointerWidth::HOST,
                            &options.unavailable_imports,
                            codegen::CodegenUnit::new(index, count),
                            debug,
                        )?,
                        None => codegen::Module::build(
                            program,
                            &format!("{}.{index}", options.module_name),
                            kira_runtime_abi::ForeignPointerWidth::HOST,
                            &options.unavailable_imports,
                            codegen::CodegenUnit::new(index, count),
                        )?,
                    };
                    module.emit_object(path, options.optimize)
                })
            })
            .collect();
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
    let objects = emit_codegen_units(program, options, debug)?;

    let executable = match &options.executable_path {
        Some(path) => {
            let llvm = kira_toolchain::discover(None)?;
            kira_diagnostics::progress!("compiling the foreign shim");
            let shim = build_foreign_shim(
                program,
                &options.unavailable_imports,
                &options.object_path,
                &llvm,
            )?;
            kira_diagnostics::progress!("linking {}", path.display());
            match debug {
                Some(debug) => link::link_executable_debug(
                    &llvm,
                    &objects,
                    &options.runtime_archive,
                    &options.foreign_link,
                    shim.as_ref().map(|shim| shim.object.as_path()),
                    path,
                    &debug_symbols(program, debug, true),
                )?,
                None => link::link_executable(
                    &llvm,
                    &objects,
                    &options.runtime_archive,
                    &options.foreign_link,
                    shim.as_ref().map(|shim| shim.object.as_path()),
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
    /// The resolved C link inputs that satisfy the program's foreign imports:
    /// archives in link order plus the frameworks, system libraries, and
    /// linker flags declared beside them.
    pub foreign_link: NativeLinkInputs,
    /// Imports whose library this target does not have.
    ///
    /// Their adapters answer a status instead of calling a symbol nothing
    /// defines. Without this the sidecar references every import's C symbol and
    /// the *link* fails — naming Vulkan and Direct3D entry points on a machine
    /// that was never going to have them — even though the program's own
    /// declarations already said those libraries are optional.
    pub unavailable_imports: Vec<usize>,
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
    let module = codegen::Module::build_adapter_sidecar(
        program,
        &options.module_name,
        &options.unavailable_imports,
    )?;
    kira_diagnostics::progress!("emitting object");
    module.emit_object(&options.object_path, false)?;
    let llvm = kira_toolchain::discover(None)?;
    // Both the adapters and the callback thunks: a thunk is referenced by
    // nothing inside the sidecar — C is what calls it — so without forcing it by
    // name the linker is free to drop the very symbol the host resolves.
    let adapter_symbols: Vec<String> = (0..program.foreign_imports.len())
        .map(adapter_name)
        .chain((0..program.foreign_callbacks.len()).map(callback_name))
        .collect();
    let shim = build_foreign_shim(
        program,
        &options.unavailable_imports,
        &options.object_path,
        &llvm,
    )?;
    link::link_adapter_sidecar(
        &llvm,
        &options.object_path,
        &options.runtime_archive,
        &options.foreign_link,
        shim.as_ref().map(|shim| shim.object.as_path()),
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
    object_path: &Path,
    device: kira_backend_api::WasmDevice,
) -> Result<(), LlvmError> {
    // A wasm module lays out a pointer in four bytes whatever host builds it,
    // and the aggregate offsets are computed during lowering.
    let width = match device {
        kira_backend_api::WasmDevice::Wasm32 => kira_runtime_abi::ForeignPointerWidth::Bits32,
        kira_backend_api::WasmDevice::Wasm64 => kira_runtime_abi::ForeignPointerWidth::Bits64,
    };
    let module = codegen::Module::build(
        program,
        module_name,
        width,
        &[],
        codegen::CodegenUnit::WHOLE,
    )?;
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
    kira_diagnostics::progress!("emitting object");
    module.emit_object(&options.object_path, options.optimize)?;

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

fn build_hybrid_library_inner(
    program: &IrProgram,
    options: &NativeBuildOptions,
    debug: Option<&DebugInfo>,
) -> Result<HybridArtifacts, LlvmError> {
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
    kira_diagnostics::progress!("emitting object");
    module.emit_object(&options.object_path, options.optimize)?;

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
    // Both the adapters and the callback thunks: a thunk is referenced by
    // nothing inside the sidecar — C is what calls it — so without forcing it by
    // name the linker is free to drop the very symbol the host resolves.
    let adapter_symbols: Vec<String> = (0..program.foreign_imports.len())
        .map(adapter_name)
        .chain((0..program.foreign_callbacks.len()).map(callback_name))
        .collect();
    let shim = build_foreign_shim(
        program,
        &options.unavailable_imports,
        &options.object_path,
        &llvm,
    )?;
    match debug {
        Some(debug) => link::link_hybrid_library_debug(
            &llvm,
            &options.object_path,
            &options.runtime_archive,
            &options.foreign_link,
            shim.as_ref().map(|shim| shim.object.as_path()),
            &adapter_symbols,
            &library,
            &debug_symbols(program, debug, false),
        )?,
        None => link::link_hybrid_library(
            &llvm,
            &options.object_path,
            &options.runtime_archive,
            &options.foreign_link,
            shim.as_ref().map(|shim| shim.object.as_path()),
            &adapter_symbols,
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
