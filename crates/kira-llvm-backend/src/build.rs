//! Native executable, library, and WebAssembly object builds.

use std::path::{Path, PathBuf};

use kira_backend_api::NativeTarget;
use kira_debug::DebugInfo;
use kira_ir::IrProgram;

use super::NativeBuildTarget;
use super::{LlvmError, NativeArtifacts, NativeBuildOptions, codegen};
use super::{NativeExportSurface, NativeLinkInputs};
use crate::{link, reachability};

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
pub(super) fn pointer_width_for(target: &NativeTarget) -> kira_runtime_abi::ForeignPointerWidth {
    match target.cross() {
        None => kira_runtime_abi::ForeignPointerWidth::HOST,
        Some(cross) => cross.pointer_width(),
    }
}

/// Adds the Rust helper archive and bundled DLL used by native libffi calls to
/// the same link/staging set every native backend consumes.
pub(super) fn ffi_link_inputs(
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

/// Selects exactly the body symbols emitted by a debug native module.
pub(super) fn debug_symbols(
    program: &IrProgram,
    info: &DebugInfo,
    executable: bool,
) -> Vec<String> {
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
