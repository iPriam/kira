//! `kira debug --prepare`: build a debug target and describe it, without
//! running it.
//!
//! A debugger frontend that owns its own session — an editor, or the LLDB MCP
//! server — needs what `kira debug` builds and none of what it then does with
//! it. This prints one JSON object describing the built target: the executable
//! to launch, the arguments that host it, the function identities a breakpoint
//! resolves against, and the VM probe on the backends that interpret bytecode.

use std::path::{Path, PathBuf};

use kira_backend_api::BackendMode;
use kira_debug::{DebugInfo, Execution as PreparedExecution, PreparedTarget};
use kira_ir::IrProgram;
use kira_llvm_backend::NativeLinkInputs;
use kira_runtime_abi::Execution;

use super::{DebugOptions, hybrid_host_arguments, vm_lldb};
use crate::hybrid;
use crate::native;
use crate::pipeline::{EXIT_FAILURE, EXIT_OK};
use crate::progress::{err, out};

/// Builds the target `options` selects and prints its description.
pub(super) fn run(
    ir: &IrProgram,
    source: &Path,
    foreign_link: &NativeLinkInputs,
    options: &DebugOptions,
    info: &DebugInfo,
) -> i32 {
    let prepared = match options.compile.backend {
        BackendMode::VmBytecode => vm(ir, source, foreign_link, options, info),
        BackendMode::Hybrid => hybrid_target(ir, source, foreign_link, options, info),
        BackendMode::LlvmNative => llvm(ir, source, foreign_link, options, info),
    };
    let prepared = match prepared {
        Ok(prepared) => prepared,
        Err(error) => {
            err!("kira debug: {error}");
            return EXIT_FAILURE;
        }
    };
    match serde_json::to_string(&prepared) {
        Ok(text) => {
            out!("{text}");
            EXIT_OK
        }
        Err(error) => {
            err!("kira debug: cannot describe the prepared target: {error}");
            EXIT_FAILURE
        }
    }
}

/// Compiles bytecode and describes the VM host that will interpret it.
///
/// The module and direct binding manifest outlive this command, because the
/// session that debugs them starts after it returns. They are listed as the
/// target's artifacts, which is what the frontend removes when its session ends.
fn vm(
    ir: &IrProgram,
    source: &Path,
    foreign_link: &NativeLinkInputs,
    options: &DebugOptions,
    info: &DebugInfo,
) -> Result<PreparedTarget, String> {
    let module = kira_bytecode::compile(ir)
        .map_err(|error| format!("bytecode compilation failed: {error}"))?;
    let module_path = vm_lldb::temporary_module_path(source);
    std::fs::write(&module_path, module.to_bytes()).map_err(|error| {
        format!(
            "cannot write the VM module `{}`: {error}",
            module_path.display()
        )
    })?;
    let binding_paths = if ir.foreign_imports.is_empty() {
        None
    } else {
        let bindings =
            native::direct_foreign_bindings(ir, source, foreign_link).map_err(|error| {
                let _ = std::fs::remove_file(&module_path);
                error.to_string()
            })?;
        let path = vm_lldb::temporary_binding_path(source);
        native::write_foreign_binding_paths(&path, &bindings).map_err(|error| {
            let _ = std::fs::remove_file(&module_path);
            let _ = std::fs::remove_file(&path);
            error.to_string()
        })?;
        Some(path)
    };
    let host = host_executable().inspect_err(|_| {
        let _ = std::fs::remove_file(&module_path);
    })?;
    let arguments = vm_lldb::vm_host_arguments(&module_path, binding_paths.as_deref(), options);
    let mut artifacts = vec![module_path];
    if let Some(binding_paths) = &binding_paths {
        artifacts.push(binding_paths.clone());
    }
    Ok(PreparedTarget::new(info, host)
        .with_arguments(arguments)
        .with_artifacts(artifacts))
}

/// Builds a hybrid bundle and describes the host that loads it.
fn hybrid_target(
    ir: &IrProgram,
    source: &Path,
    foreign_link: &NativeLinkInputs,
    options: &DebugOptions,
    info: &DebugInfo,
) -> Result<PreparedTarget, String> {
    let bundle = hybrid::build_debug(ir, source, options.compile.emit_llvm_ir, foreign_link, info)
        .map_err(|error| error.to_string())?;
    let host = host_executable()?;
    let arguments = hybrid_host_arguments(&bundle.manifest, source, options);
    let manifest = super::read_hybrid_manifest(&bundle.manifest)?;
    let mut target = PreparedTarget::new(info, host).with_arguments(arguments);
    // A hybrid build splits its functions between the two engines, and only the
    // manifest knows which went where. A bytecode function that kept a native
    // symbol here would be broken on at an address that has no body, so the
    // symbol is dropped and the function is reached through the VM probe.
    for function in &mut target.functions {
        let native = manifest
            .functions
            .iter()
            .find(|entry| entry.id == function.id)
            .is_some_and(|entry| entry.execution == Execution::Native);
        if !native {
            function.symbol = None;
            function.execution = PreparedExecution::Bytecode;
        }
    }
    Ok(target)
}

/// Builds a native executable with debug metadata and describes it.
fn llvm(
    ir: &IrProgram,
    source: &Path,
    foreign_link: &NativeLinkInputs,
    options: &DebugOptions,
    info: &DebugInfo,
) -> Result<PreparedTarget, String> {
    let artifacts = native::build_debug(
        ir,
        source,
        options.compile.emit_llvm_ir,
        options.compile.release,
        foreign_link,
        info,
    )
    .map_err(|error| error.to_string())?;
    let executable = artifacts
        .executable
        .ok_or_else(|| "the LLVM build produced no executable".to_owned())?;
    Ok(PreparedTarget::new(info, executable)
        .with_arguments(options.compile.program_arguments.clone()))
}

/// The `kira` executable that hosts a VM or hybrid debug session.
fn host_executable() -> Result<PathBuf, String> {
    std::env::current_exe()
        .map_err(|error| format!("cannot locate the debug host executable: {error}"))
}
