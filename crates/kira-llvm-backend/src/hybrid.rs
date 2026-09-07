//! Hybrid native-half builds and their exported trampoline surface.

use std::path::PathBuf;

use kira_debug::DebugInfo;
use kira_ir::IrProgram;

use super::build::{debug_symbols, ffi_link_inputs, pointer_width_for};
use super::callback_name;
use super::{LlvmError, NativeBuildOptions, codegen};
use crate::{link, reachability};

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
    let object_path = emit_hybrid_object(program, options, debug)?;
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
        Some(debug) => {
            let symbols = debug_symbols(program, debug, false);
            link::link_hybrid_library(
                &llvm,
                &object_path,
                &options.runtime_archive,
                &foreign_link,
                &library,
                link::HybridLinkOptions {
                    retained_symbols: &retained_symbols,
                    debug_symbols: Some(&symbols),
                    sanitize: options.sanitize,
                },
            )?
        }
        None => link::link_hybrid_library(
            &llvm,
            &object_path,
            &options.runtime_archive,
            &foreign_link,
            &library,
            link::HybridLinkOptions {
                retained_symbols: &retained_symbols,
                debug_symbols: None,
                sanitize: options.sanitize,
            },
        )?,
    }

    Ok(HybridArtifacts {
        object: object_path,
        library,
        exports: exported_trampolines(program),
    })
}

/// Compiles the native half of a hybrid program to a bare object and links
/// nothing.
///
/// This is the embedded-application form: instead of a self-contained shared
/// library, the object's trampolines and adapters are linked *into* a host
/// binary (an exported Xcode app), which also carries the runtime archive the
/// helpers live in. The returned exports are the full trampoline surface,
/// because the host's hybrid manifest records it exactly as the library form
/// does.
///
/// Returns the object path and one `(function id, trampoline symbol)` pair
/// per native function, in manifest order.
pub fn build_hybrid_object(
    program: &IrProgram,
    options: &NativeBuildOptions,
) -> Result<(PathBuf, Vec<(u32, String)>), LlvmError> {
    let module = codegen::Module::build_hybrid_for_target(
        program,
        &options.module_name,
        &options.unavailable_imports,
        pointer_width_for(options.target.target()),
        options.target.target().clone(),
    )?;
    if let Some(path) = &options.ir_path {
        module.write_ir(path)?;
    }
    module.emit_object(&options.object_path, options.optimize, options.sanitize)?;
    let object_path = options.object_path.clone();
    Ok((object_path, exported_trampolines(program)))
}

/// Lowers and emits the hybrid native half's object, shared by both forms.
fn emit_hybrid_object(
    program: &IrProgram,
    options: &NativeBuildOptions,
    debug: Option<&DebugInfo>,
) -> Result<PathBuf, LlvmError> {
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
    module.emit_object(&options.object_path, options.optimize, options.sanitize)?;
    Ok(options.object_path.clone())
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
