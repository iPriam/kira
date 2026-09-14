//! The `kira debug` command and its three backend adapters.

use std::path::{Path, PathBuf};

use kira_backend_api::BackendMode;
use kira_debug::{
    Backend, DebugInfo, LldbDapBreakpoint, LldbDapLaunch, LldbLaunch, VM_PROBE_SYMBOL,
    VM_TEXT_SYMBOL, VmDebugger, VmDebuggerMode,
};
use kira_hybrid_definition::{HybridFunction, HybridManifest};
use kira_ir::IrProgram;
use kira_llvm_backend::NativeLinkInputs;
use kira_main::StdoutHost;
use kira_runtime_abi::{Execution, NativeStateHost, env};
use kira_vm_runtime::VmLldbObserver;

use crate::hybrid;
use crate::native;
use crate::options::{CompileOptions, OptionsError};
use crate::pipeline::{EXIT_FAILURE, EXIT_OK};
use crate::progress::{err, out};

mod prepare;
mod vm_lldb;

pub(crate) use vm_lldb::run_host as run_vm_host;

/// Private argv verb used when LLDB launches the current `kira` executable as
/// the host for a debug hybrid bundle. It is intentionally absent from the
/// public command table: users select this through `kira debug --lldb`.
const HYBRID_DEBUG_HOST: &str = "__hybrid-debug-host";
/// Private argv verb used when LLDB launches the VM host.
const VM_DEBUG_HOST: &str = "__vm-debug-host";

/// Options specific to a debugger session.
#[derive(Debug, Clone, PartialEq)]
pub struct DebugOptions {
    /// The normal compiler/backend options.
    pub compile: CompileOptions,
    /// Function or function/program-counter breakpoints.
    pub breakpoints: Vec<String>,
    /// Whether the backend should run without an interactive prompt.
    pub batch: bool,
    /// Whether native LLDB or VM stops should print an instruction window.
    pub disassemble: bool,
    /// Whether the run should be hosted by a real LLDB process.
    ///
    /// LLVM/native debugging already uses LLDB unconditionally. VM debugging
    /// exposes a stable native probe frame; hybrid debugging combines that VM
    /// probe with native shared-library symbols.
    pub lldb: bool,
    /// Whether a VM run should use the real LLDB Debug Adapter Protocol.
    ///
    /// This is the stable multi-stop frontend on Windows toolchains whose
    /// command interpreter aborts while resuming a second native probe.
    pub lldb_dap: bool,
    /// Number of explicit DAP `continue` requests after the first stop.
    pub dap_continues: usize,
    /// Whether to build and describe the target instead of debugging it.
    ///
    /// A frontend that owns its own debugger session builds through this and
    /// then drives LLDB itself, so the compiler is asked for artifacts and
    /// identities rather than for a transcript.
    pub prepare: bool,
}

/// Why `kira debug` arguments were rejected.
#[derive(Debug, thiserror::Error)]
pub enum DebugOptionsError {
    /// A breakpoint flag did not have a value.
    #[error("`--break` expects a function name or function:instruction")]
    BreakpointMissingValue,
    /// A DAP resume count did not have a value.
    #[error("`--dap-continues` expects a non-negative integer")]
    DapContinuesMissingValue,
    /// A DAP resume count was not an integer.
    #[error("`--dap-continues` expects a non-negative integer")]
    DapContinuesInvalidValue,
    /// The shared compiler options rejected the remaining arguments.
    #[error(transparent)]
    Compile(#[from] OptionsError),
}

/// Parses debugger flags and delegates shared flags to `CompileOptions`.
pub fn parse(args: &[String]) -> Result<DebugOptions, DebugOptionsError> {
    let mut compile_args = Vec::with_capacity(args.len());
    let mut breakpoints = Vec::new();
    let mut batch = false;
    let mut disassemble = true;
    let mut lldb = false;
    let mut lldb_dap = false;
    let mut dap_continues = 0;
    let mut prepare = false;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--break" | "-b" => {
                let value = args
                    .get(index + 1)
                    .ok_or(DebugOptionsError::BreakpointMissingValue)?;
                breakpoints.push(value.clone());
                index += 1;
            }
            value if value.starts_with("--break=") => {
                let value = value.trim_start_matches("--break=");
                if value.is_empty() {
                    return Err(DebugOptionsError::BreakpointMissingValue);
                }
                breakpoints.push(value.to_owned());
            }
            "--batch" => batch = true,
            "--disassemble" => disassemble = true,
            "--no-disassemble" => disassemble = false,
            "--lldb" => lldb = true,
            "--lldb-dap" => lldb_dap = true,
            "--prepare" => prepare = true,
            "--dap-continues" => {
                let value = args
                    .get(index + 1)
                    .ok_or(DebugOptionsError::DapContinuesMissingValue)?;
                dap_continues = value
                    .parse()
                    .map_err(|_| DebugOptionsError::DapContinuesInvalidValue)?;
                lldb_dap = true;
                index += 1;
            }
            value if value.starts_with("--dap-continues=") => {
                let value = value.trim_start_matches("--dap-continues=");
                if value.is_empty() {
                    return Err(DebugOptionsError::DapContinuesMissingValue);
                }
                dap_continues = value
                    .parse()
                    .map_err(|_| DebugOptionsError::DapContinuesInvalidValue)?;
                lldb_dap = true;
            }
            other => compile_args.push(other.to_owned()),
        }
        index += 1;
    }
    Ok(DebugOptions {
        compile: CompileOptions::parse(&compile_args)?,
        breakpoints,
        batch,
        disassemble,
        lldb,
        lldb_dap,
        dap_continues,
        prepare,
    })
}

/// Builds the target and describes it without starting a debugger.
pub fn prepare_target(
    ir: &IrProgram,
    source: &Path,
    foreign_link: &NativeLinkInputs,
    options: &DebugOptions,
    info: &DebugInfo,
) -> i32 {
    prepare::run(ir, source, foreign_link, options, info)
}

/// Warns before an LLDB session on a host that will never authorize one.
///
/// A warning rather than a refusal: a desktop session may still grant the
/// taskport right through its own prompt, and refusing would break exactly
/// the machine that can answer it. Headless hosts get the one line that
/// explains an otherwise-silent hang.
fn warn_when_debugging_unauthorized() {
    if kira_debug::debugging_unauthorized() {
        err!("kira debug: {}", kira_debug::ENABLE_DEBUGGING_HINT);
    }
}

/// Runs a verified IR program under the VM debugger or real LLDB.
pub fn run_vm(
    ir: &IrProgram,
    source: &Path,
    foreign_link: &NativeLinkInputs,
    options: &DebugOptions,
    info: &DebugInfo,
) -> i32 {
    // The same gate `run` and `test` apply, asked before anything is compiled: a
    // debugger session on a program the interpreter cannot serve would stop at
    // the call with the session already open, which is the worst place to learn
    // that the engine was the wrong one.
    let refused = crate::pipeline::unservable_syscalls(ir);
    if !refused.is_empty() {
        err!("kira debug: {}", crate::pipeline::syscall_refusal(&refused));
        return EXIT_FAILURE;
    }
    let module = match kira_bytecode::compile(ir) {
        Ok(module) => module,
        Err(error) => {
            err!("kira debug: bytecode compilation failed: {error}");
            return EXIT_FAILURE;
        }
    };
    if options.lldb_dap {
        warn_when_debugging_unauthorized();
        return vm_lldb::run_under_lldb_dap(ir, &module, source, foreign_link, options, info);
    }
    if options.lldb {
        warn_when_debugging_unauthorized();
        return vm_lldb::run_under_lldb(ir, &module, source, foreign_link, options, info);
    }
    let mode = if options.batch {
        VmDebuggerMode::Batch
    } else {
        VmDebuggerMode::Interactive
    };
    let mut debugger = VmDebugger::new(mode);
    debugger.set_disassemble_on_stop(options.disassemble);
    debugger.set_source_info(info);
    for value in &options.breakpoints {
        if !debugger.add_breakpoint_text(value) {
            err!("kira debug: invalid breakpoint `{value}`");
            return EXIT_FAILURE;
        }
    }

    if ir.foreign_imports.is_empty() && ir.foreign_callbacks.is_empty() {
        // SAFETY: the CLI owns this debugger run and does not access the
        // process environment from another thread while the VM executes.
        return unsafe {
            env::with_arguments(&options.compile.program_arguments, || {
                let mut host = NativeStateHost::new(StdoutHost);
                let result = kira_vm_runtime::execute_with_main_thread_debug(
                    &module,
                    &mut host,
                    &mut debugger,
                );
                match result {
                    Ok(_) => EXIT_OK,
                    Err(error) => runtime_error(error),
                }
            })
        };
    }

    let imports = match native::direct_foreign_bindings(ir, source, foreign_link) {
        Ok(imports) => imports,
        Err(error) => {
            err!("kira: {error}");
            return EXIT_FAILURE;
        }
    };
    let program = match kira_vm_runtime::Program::load(module) {
        Ok(program) => program,
        Err(error) => {
            err!("kira: {error}");
            return EXIT_FAILURE;
        }
    };
    let session = match kira_main::ForeignSession::load_dynamic(
        program,
        imports,
        ir.foreign_callbacks
            .iter()
            .map(|callback| callback.signature().clone())
            .collect(),
        ir.foreign_aggregates.clone(),
    ) {
        Ok(session) => session,
        Err(error) => {
            err!("kira: cannot load the direct foreign-library session: {error}");
            return EXIT_FAILURE;
        }
    };
    // SAFETY: this is the same CLI-owned debugger boundary with direct
    // foreign libraries loaded.
    match unsafe {
        env::with_arguments(&options.compile.program_arguments, || {
            session.run_with_debug(&mut debugger)
        })
    } {
        Ok(_) => EXIT_OK,
        Err(error) => runtime_error(error),
    }
}

mod hybrid_debug;
pub use hybrid_debug::run_hybrid;
pub(crate) use hybrid_debug::run_hybrid_host;

/// Builds a native executable with debug metadata and hands it to real LLDB.
pub fn run_llvm(
    ir: &IrProgram,
    source: &Path,
    foreign_link: &NativeLinkInputs,
    options: &DebugOptions,
    info: &DebugInfo,
) -> i32 {
    let artifacts = match native::build_debug(
        ir,
        source,
        options.compile.emit_llvm_ir,
        options.compile.release,
        foreign_link,
        info,
        options.compile.sanitize,
    ) {
        Ok(artifacts) => artifacts,
        Err(error) => {
            err!("kira: {error}");
            return EXIT_FAILURE;
        }
    };
    let Some(target) = artifacts.executable else {
        err!("kira debug: the LLVM build produced no executable");
        return EXIT_FAILURE;
    };
    warn_when_debugging_unauthorized();
    if options.lldb_dap {
        return run_llvm_under_lldb_dap(ir.main, source, options, info, target);
    }
    let mut launch = LldbLaunch::from_info(&target, info);
    launch.breakpoints.clear();
    let symbols = match llvm_breakpoint_symbols(ir.main, &options.breakpoints, info) {
        Ok(symbols) => symbols,
        Err(error) => {
            err!("kira debug: {error}");
            return EXIT_FAILURE;
        }
    };
    for symbol in &symbols {
        launch.add_breakpoint(symbol);
    }
    launch.disassemble = options.disassemble;
    launch.batch = options.batch;
    launch.arguments = options.compile.program_arguments.clone();
    print_llvm_source_context(source, info, &launch.breakpoints);
    out!("LLDB target: {}", target.display());
    match launch.launch() {
        Ok(output) => {
            if !output.stdout.is_empty() {
                print!("{}", output.stdout);
            }
            if !output.stderr.is_empty() {
                eprint!("{}", output.stderr);
            }
            EXIT_OK
        }
        Err(error) => {
            err!("kira: {error}");
            EXIT_FAILURE
        }
    }
}

fn run_llvm_under_lldb_dap(
    main: Option<u32>,
    source: &Path,
    options: &DebugOptions,
    info: &DebugInfo,
    target: PathBuf,
) -> i32 {
    let symbols = match llvm_breakpoint_symbols(main, &options.breakpoints, info) {
        Ok(symbols) => symbols,
        Err(error) => {
            err!("kira debug: {error}");
            return EXIT_FAILURE;
        }
    };
    let mut launch = LldbDapLaunch::new(&target);
    for symbol in &symbols {
        launch.add_breakpoint(LldbDapBreakpoint::new(symbol));
    }
    launch.set_disassemble(options.disassemble);
    launch.set_continue_count(options.dap_continues);
    launch.arguments = options.compile.program_arguments.clone();
    print_llvm_source_context(source, info, &symbols);
    out!("LLDB DAP target: {}", target.display());
    run_dap_launch(launch)
}

fn llvm_breakpoint_symbols(
    main: Option<u32>,
    requested: &[String],
    info: &DebugInfo,
) -> Result<Vec<String>, String> {
    if requested.is_empty() {
        return Ok(main
            .and_then(|id| info.functions.iter().find(|function| function.id == id))
            .and_then(|function| function.symbol.clone())
            .into_iter()
            .collect());
    }
    requested
        .iter()
        .map(|requested| {
            let name = hybrid_debug::breakpoint_function_name(requested);
            let Some(function) = info.functions.iter().find(|function| {
                function.name == name
                    || function.symbol.as_deref() == Some(name)
                    || function.id.to_string() == name
            }) else {
                return Err(format!("no LLVM function matches breakpoint `{requested}`"));
            };
            function
                .symbol
                .clone()
                .ok_or_else(|| format!("`{name}` has no native body in this build"))
        })
        .collect()
}

fn run_dap_launch(launch: LldbDapLaunch) -> i32 {
    match launch.launch() {
        Ok(output) => {
            if !output.stdout.is_empty() {
                print!("{}", output.stdout);
            }
            if !output.stderr.is_empty() {
                eprint!("{}", output.stderr);
            }
            EXIT_OK
        }
        Err(error) => {
            err!("kira: {error}");
            EXIT_FAILURE
        }
    }
}

/// Prints a source anchor even when a platform LLDB cannot load its native
/// symbol companion. The DWARF/CodeView records remain in the artifact; this
/// small CLI-side view keeps the first stopped Kira location visible in the
/// same transcript on Windows, where the bundled LLDB may not have PDB support.
fn print_llvm_source_context(source: &Path, info: &DebugInfo, breakpoints: &[String]) {
    let Ok(text) = std::fs::read_to_string(source) else {
        return;
    };
    let lines = text.lines().collect::<Vec<_>>();
    for function in info.functions.iter().filter(|function| {
        function
            .symbol
            .as_deref()
            .is_some_and(|symbol| breakpoints.iter().any(|breakpoint| breakpoint == symbol))
    }) {
        let line = function.line.max(1);
        let text = lines
            .get(line.saturating_sub(1) as usize)
            .copied()
            .unwrap_or("");
        out!(
            "source: {}:{line} ({}) | {text}",
            source.display(),
            function.name
        );
    }
}

fn runtime_error(error: impl std::fmt::Display) -> i32 {
    err!("kira: runtime trap: {error}");
    EXIT_FAILURE
}

/// Maps compiler backend selection to shared debugger metadata.
#[must_use]
pub fn backend(mode: BackendMode) -> Backend {
    match mode {
        BackendMode::VmBytecode => Backend::Vm,
        BackendMode::Hybrid => Backend::Hybrid,
        BackendMode::LlvmNative => Backend::Llvm,
    }
}
