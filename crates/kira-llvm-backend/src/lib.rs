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

use kira_backend_api::NativeTarget;
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
// Public because a shim object is an input to a link line rather than an
// internal step: whoever assembles that line — this crate for a native program,
// the CLI for the emscripten link — needs to name the object it produced. It
// was neither declared nor compiled until the target-aware link went in, which
// is also why its own test had never run.
pub mod shim_build;
#[cfg(test)]
mod shim_tests;

pub use exports::{NativeClass, NativeExport, NativeExportSurface};
pub use link::{LinkError, NativeBuildTarget, SYSROOT_VARIABLE, link_ffi_carrier};
pub use platform::{PLATFORM_LINK_LISTS, PlatformLinkList, host_link_list, link_list_for};

/// Reports whether this compiler can emit machine code for `target`.
///
/// What a compiler can emit for is fixed when it is linked: the managed LLVM
/// bundle carries a set of code generators, and one it was not built with is a
/// set of symbols that are simply not in the binary. That is knowable before a
/// program is read, which is why this exists separately from the build — a
/// caller asks it first, and a machine that cannot serve the request says so
/// before the user goes and arranges a sysroot and a runtime archive for a build
/// that could never have finished.
///
/// [`NativeTarget::Host`] is always supported: a bundle without this host's own
/// code generator is refused by the backend's build script, since it could emit
/// for nothing at all.
pub fn supports_target(target: &NativeTarget) -> Result<(), LlvmError> {
    match target.cross() {
        None => Ok(()),
        Some(cross) => codegen::check_supported(cross),
    }
}

/// The symbol a foreign import's adapter is bound under.
///
/// The spelling lives in the runtime ABI crate and is what a hybrid manifest
/// records per import. The LLVM backend does **not** define this symbol: both
/// live hosts bind imports through libffi closures generated at load time,
/// which carry these names. An emitter that wants to define adapters natively
/// again owns this contract afresh.
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
/// [`callback_body_name`] for one whose by-value struct the shim classifies —
/// the same split [`crate::shim::callback_needs_entry`], so the name emitted
/// here and the entry the shim generates always agree.
/// Always [`callback_name`] today. A signature taking a struct by value is
/// *meant* to split — shim entry under [`callback_name`] with the true C
/// prototype, LLVM body under [`callback_body_name`] — but the body this
/// backend emits carries libffi's closure signature
/// `(cif, result, arguments, user_data)`, which the shim's positional forward
/// call does not speak. Renaming the emission without a second,
/// true-prototype body would trade today's duplicate-symbol link error for
/// silent miscompiled arguments, so the split stays unimplemented until that
/// body exists. See `.codex/work/wasm-callback-shims.md`.
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
    /// This compiler was built against a managed LLVM carrying no code
    /// generator for the requested target's architecture.
    ///
    /// The counterpart of [`LlvmError::WasmTargetMissing`], and reported the
    /// same way and for the same reason: what a bundle can emit for is decided
    /// when the bundle is built, so a cross target it was not built with has to
    /// be refused by name at the point it is asked for. Every bundle published
    /// before `llvm-metadata.toml` named X86 and AArch64 outright carries only
    /// the generator of the machine that built it, and this is what those
    /// bundles say when asked for anything else.
    #[error(
        "this compiler was built against a managed LLVM without the {generator} \
         code generator, so it cannot emit code for `{target}`; install a bundle \
         built with the targets `llvm-metadata.toml` pins \
         (`knvm install-llvm --force`) and rebuild the compiler against it"
    )]
    TargetCodeGeneratorMissing {
        /// The target that was asked for, in Kira's `arch-os-abi` spelling.
        target: String,
        /// LLVM's name for the code generator that architecture needs.
        generator: &'static str,
    },
    /// The requested target names an architecture Kira has no code generator
    /// for, whatever the linked bundle carries.
    #[error(
        "`{target}` names architecture `{arch}`, which Kira has no LLVM code \
         generator for; the architectures it can emit for are `x86_64`, `x86`, \
         and `aarch64`"
    )]
    TargetArchitectureUnknown {
        /// The target that was asked for, in Kira's `arch-os-abi` spelling.
        target: String,
        /// Its architecture component.
        arch: String,
    },
    /// Two declarations want the same symbol name with different signatures —
    /// a foreign import colliding with a maths declaration, or two imports
    /// of one C symbol at different types. One flat namespace cannot hold
    /// both, and calling through the wrong signature fails module
    /// verification far from the line that caused it.
    #[error(
        "the symbol `{symbol}` is already declared with a different signature; \
         a foreign import and a builtin maths call are competing for one name"
    )]
    SymbolCollision {
        /// The symbol both declarations want.
        symbol: String,
    },
    /// LLVM refused the normalized triple for a target whose code generator is
    /// linked and registered.
    #[error(
        "LLVM does not recognize `{triple}`, the toolchain spelling of target \
         `{target}`"
    )]
    TargetTripleUnknown {
        /// The target that was asked for, in Kira's `arch-os-abi` spelling.
        target: String,
        /// The `arch-vendor-os-abi` triple LLVM was asked to resolve.
        triple: String,
    },
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
    /// Which machine this build emits and links for, and where that machine's
    /// system libraries live.
    ///
    /// [`NativeBuildTarget::host`] is the default and what every build that
    /// produces something *this* process loads must use. It decides three
    /// things at once, and they have to be one value rather than three because
    /// disagreeing is silent: the data layout the lowering computes offsets
    /// against, the code generator the object comes out of, and the machine the
    /// link line is aimed at.
    pub target: NativeBuildTarget,
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
    target: &NativeBuildTarget,
) -> Result<codegen::Module, LlvmError> {
    let native = target.target();
    let width = pointer_width_for(native);
    match kind {
        NativeModuleKind::Executable => match debug {
            Some(debug) => codegen::Module::build_debug(
                program,
                module_name,
                width,
                unavailable,
                unit,
                native,
                debug,
            ),
            None => codegen::Module::build(program, module_name, width, unavailable, unit, native),
        },
        // A live library is loaded into this process, so it is the host's by
        // construction and takes no target of its own.
        NativeModuleKind::LiveLibrary => {
            codegen::Module::build_native_live(program, module_name, unavailable, unit)
        }
    }
}

/// The pointer width `target`'s C-layout aggregates are laid out at.
///
/// Every offset the lowering computes is baked in at this width, so a build for
/// a machine whose pointers are a different size from this one's has to say so
/// before the first field offset is worked out — not after the module has
/// already been laid out.
fn pointer_width_for(target: &NativeTarget) -> kira_runtime_abi::ForeignPointerWidth {
    match target.cross() {
        None => kira_runtime_abi::ForeignPointerWidth::HOST,
        Some(cross) => cross.pointer_width(),
    }
}

/// Adds the Rust helper archive and bundled DLL used by native libffi calls to
/// the same link/staging set every native backend consumes.
fn ffi_link_inputs(
    program: &IrProgram,
    foreign_link: &NativeLinkInputs,
    unavailable: &[usize],
) -> Result<NativeLinkInputs, LlvmError> {
    // A system call needs none of this. It is an instruction, so there is no
    // address for libffi to call through and no shared object for the bundled
    // one to be found in — and dragging libffi onto the link line for a program
    // whose only foreign calls are system calls is exactly the failure this
    // capability exists to avoid: the freestanding image would carry a
    // dependency on `libffi.so.8` and be refused by a kernel that has no loader
    // to find it.
    let has_callable_foreign = program
        .foreign_imports
        .iter()
        .enumerate()
        .any(|(index, entry)| {
            entry.import.abi().binds_a_library_symbol() && !unavailable.contains(&index)
        });
    if !has_callable_foreign && program.foreign_callbacks.is_empty() {
        return Ok(foreign_link.clone());
    }
    let mut link = foreign_link.clone();
    // The helper archive carries libffi itself, linked in: there is no engine
    // file to put beside the artifact, and therefore nothing for the artifact
    // to find at run time.
    link.push_archive(kira_libffi::runtime_archive()?);
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
            &options.target,
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
                        &options.target,
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
                    &options.target,
                )?,
                None => link::link_executable(
                    &llvm,
                    &objects,
                    &options.runtime_archive,
                    &foreign_link,
                    path,
                    &options.target,
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
    let module = codegen::Module::build_library(
        program,
        &options.module_name,
        &options.exports,
        pointer_width_for(options.target.target()),
        options.target.target(),
    )?;
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
            &options.target,
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
