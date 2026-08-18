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
        return vm_lldb::run_under_lldb_dap(ir, &module, source, foreign_link, options, info);
    }
    if options.lldb {
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
                match kira_vm_runtime::execute_with_debug(&module, &mut host, &mut debugger) {
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

/// Builds and runs a hybrid bundle with the VM debugger attached to its VM half.
pub fn run_hybrid(
    ir: &IrProgram,
    source: &Path,
    foreign_link: &NativeLinkInputs,
    options: &DebugOptions,
    info: &DebugInfo,
) -> i32 {
    let bundle =
        match hybrid::build_debug(ir, source, options.compile.emit_llvm_ir, foreign_link, info) {
            Ok(bundle) => bundle,
            Err(error) => {
                err!("kira: {error}");
                return EXIT_FAILURE;
            }
        };
    out!("hybrid debug bundle: {}", bundle.manifest.display());
    if options.lldb_dap {
        return run_hybrid_under_lldb_dap(source, options, info, &bundle);
    }
    if options.lldb {
        return run_hybrid_under_lldb(source, options, info, &bundle);
    }
    let session = match kira_hybrid_runtime::Session::load(&bundle.manifest) {
        Ok(session) => session,
        Err(error) => {
            err!("kira: {error}");
            return EXIT_FAILURE;
        }
    };
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
    // SAFETY: the debugger owns this Hybrid run and keeps process-environment
    // access exclusive while the VM and native library execute.
    match unsafe {
        env::with_arguments(&options.compile.program_arguments, || {
            session.run_with_debug(&mut debugger)
        })
    } {
        Ok(()) => EXIT_OK,
        Err(error) => {
            err!("kira: {error}");
            EXIT_FAILURE
        }
    }
}

/// Runs a debug hybrid bundle under real LLDB while the VM half reports its
/// instruction stops from the launched host process.
///
/// LLDB owns the child process and can stop native functions in the loaded
/// shared library. The child also installs [`VmDebugger`] in batch mode, so a
/// runtime function breakpoint and a native function breakpoint can coexist in
/// one transcript without two consumers fighting over stdin.
fn run_hybrid_under_lldb(
    source: &Path,
    options: &DebugOptions,
    info: &DebugInfo,
    bundle: &hybrid::HybridBundle,
) -> i32 {
    let manifest = match read_hybrid_manifest(&bundle.manifest) {
        Ok(manifest) => manifest,
        Err(error) => {
            err!("kira debug: {error}");
            return EXIT_FAILURE;
        }
    };
    let target = match std::env::current_exe() {
        Ok(target) => target,
        Err(error) => {
            err!("kira debug: cannot locate the LLDB host executable: {error}");
            return EXIT_FAILURE;
        }
    };
    let mut launch = LldbLaunch::from_info(&target, info);
    launch.breakpoints.clear();
    if options.breakpoints.is_empty() {
        if let Some(entry) = manifest
            .entry
            .and_then(|id| manifest.functions.iter().find(|function| function.id == id))
            && entry.execution == Execution::Native
            && let Some(symbol) = entry.exported_name.as_deref()
        {
            launch.add_breakpoint(symbol);
        }
    } else {
        for requested in &options.breakpoints {
            let name = breakpoint_function_name(requested);
            let Some(function) = manifest_function(&manifest.functions, name) else {
                err!("kira debug: no Hybrid function matches breakpoint `{requested}`");
                return EXIT_FAILURE;
            };
            if function.execution == Execution::Native
                && let Some(symbol) = function.exported_name.as_deref()
            {
                launch.add_breakpoint(symbol);
            }
        }
    }
    launch.disassemble = options.disassemble;
    launch.batch = options.batch;
    // Swift's Windows LLDB currently aborts while unwinding some frames from
    // a hybrid DLL. The remaining post-stop queries still expose the native
    // frame, registers, and CPU instructions without taking down the session.
    launch.thread_backtrace = false;
    launch.arguments = hybrid_host_arguments(&bundle.manifest, source, options);
    print_llvm_source_context(source, info, &launch.breakpoints);
    out!("LLDB hybrid host: {}", target.display());
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

/// Runs a hybrid host through LLDB's Debug Adapter Protocol.
fn run_hybrid_under_lldb_dap(
    source: &Path,
    options: &DebugOptions,
    info: &DebugInfo,
    bundle: &hybrid::HybridBundle,
) -> i32 {
    let manifest = match read_hybrid_manifest(&bundle.manifest) {
        Ok(manifest) => manifest,
        Err(error) => {
            err!("kira debug: {error}");
            return EXIT_FAILURE;
        }
    };
    let target = match std::env::current_exe() {
        Ok(target) => target,
        Err(error) => {
            err!("kira debug: cannot locate the LLDB DAP host executable: {error}");
            return EXIT_FAILURE;
        }
    };
    let (native_symbols, needs_vm_probe) = match hybrid_breakpoints(&manifest, options) {
        Ok(breakpoints) => breakpoints,
        Err(error) => {
            err!("kira debug: {error}");
            return EXIT_FAILURE;
        }
    };
    let mut launch = LldbDapLaunch::new(&target);
    for symbol in &native_symbols {
        launch.add_breakpoint(LldbDapBreakpoint::new(symbol));
    }
    if needs_vm_probe {
        launch.add_breakpoint(LldbDapBreakpoint::new(VM_PROBE_SYMBOL));
        launch.set_text_symbol(VM_TEXT_SYMBOL);
    }
    launch.set_disassemble(options.disassemble);
    launch.set_continue_count(options.dap_continues);
    launch.arguments = hybrid_host_arguments(&bundle.manifest, source, options);
    print_llvm_source_context(source, info, &native_symbols);
    out!("LLDB DAP hybrid host: {}", target.display());
    run_dap_launch(launch)
}

/// Runs the private host command an LLDB launch uses for a hybrid session.
pub(crate) fn run_hybrid_host(args: &[String]) -> i32 {
    let options = match parse_hybrid_host_args(args) {
        Ok(options) => options,
        Err(error) => {
            err!("kira: {error}");
            return EXIT_FAILURE;
        }
    };
    let session = match kira_hybrid_runtime::Session::load(&options.manifest) {
        Ok(session) => session,
        Err(error) => {
            err!("kira: {error}");
            return EXIT_FAILURE;
        }
    };
    let functions = session
        .manifest()
        .functions
        .iter()
        .map(|function| (function.id, function.name.as_str()))
        .collect::<Vec<_>>();

    // Two observers, because a hybrid host is driven two ways. `--lldb` runs
    // this host beside an LLDB that owns the native half, and the VM half
    // reports its own stops as text. A frontend that owns the whole session
    // instead needs the stops to come through the native probe, so that one
    // debugger controls both halves.
    let result = match options.probe {
        true => {
            let breakpoints = match vm_lldb::probe_breakpoints(&options.breakpoints, &functions) {
                Ok(breakpoints) => breakpoints,
                Err(error) => {
                    err!("kira: {error}");
                    return EXIT_FAILURE;
                }
            };
            let mut observer = VmLldbObserver::with_breakpoints(breakpoints);
            // SAFETY: this private host owns the entire debugged process and
            // does not access the process environment from another thread
            // while the session runs.
            unsafe {
                env::with_arguments(&options.program_arguments, || {
                    session.run_with_debug(&mut observer)
                })
            }
        }
        false => {
            let mut debugger = VmDebugger::new(VmDebuggerMode::Batch);
            debugger.set_disassemble_on_stop(options.disassemble);
            if let Some(source) = options.source.as_deref() {
                debugger.set_source_file(source, &functions);
            }
            for breakpoint in &options.breakpoints {
                if !debugger.add_breakpoint_text(breakpoint) {
                    err!("kira: invalid VM breakpoint `{breakpoint}`");
                    return EXIT_FAILURE;
                }
            }
            // SAFETY: the same host-owned environment boundary as above.
            unsafe {
                env::with_arguments(&options.program_arguments, || {
                    session.run_with_debug(&mut debugger)
                })
            }
        }
    };
    match result {
        Ok(()) => EXIT_OK,
        Err(error) => {
            err!("kira: {error}");
            EXIT_FAILURE
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct HybridHostOptions {
    manifest: PathBuf,
    source: Option<PathBuf>,
    breakpoints: Vec<String>,
    disassemble: bool,
    /// Whether the VM half reports through the native probe rather than as text.
    probe: bool,
    program_arguments: Vec<String>,
}

fn parse_hybrid_host_args(args: &[String]) -> Result<HybridHostOptions, String> {
    let mut manifest = None;
    let mut source = None;
    let mut breakpoints = Vec::new();
    let mut disassemble = false;
    let mut probe = false;
    let mut program_arguments = Vec::new();
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--manifest" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| "`--manifest` expects a path".to_owned())?;
                manifest = Some(PathBuf::from(value));
                index += 1;
            }
            "--vm-source" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| "`--vm-source` expects a path".to_owned())?;
                source = Some(PathBuf::from(value));
                index += 1;
            }
            "--vm-break" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| "`--vm-break` expects a function or function:pc".to_owned())?;
                breakpoints.push(value.clone());
                index += 1;
            }
            "--vm-disassemble" => disassemble = true,
            "--vm-no-disassemble" => disassemble = false,
            "--vm-probe" => probe = true,
            "--" => {
                program_arguments.extend(args[index + 1..].iter().cloned());
                break;
            }
            other => return Err(format!("unknown hybrid debug host argument `{other}`")),
        }
        index += 1;
    }
    Ok(HybridHostOptions {
        manifest: manifest.ok_or_else(|| "hybrid debug host needs `--manifest`".to_owned())?,
        source,
        breakpoints,
        disassemble,
        probe,
        program_arguments,
    })
}

fn hybrid_host_arguments(manifest: &Path, source: &Path, options: &DebugOptions) -> Vec<String> {
    let mut arguments = vec![
        HYBRID_DEBUG_HOST.to_owned(),
        "--manifest".to_owned(),
        manifest.display().to_string(),
        "--vm-source".to_owned(),
        source.display().to_string(),
    ];
    for breakpoint in &options.breakpoints {
        arguments.push("--vm-break".to_owned());
        arguments.push(breakpoint.clone());
    }
    arguments.push(if options.disassemble {
        "--vm-disassemble".to_owned()
    } else {
        "--vm-no-disassemble".to_owned()
    });
    if options.prepare || options.lldb_dap {
        arguments.push("--vm-probe".to_owned());
    }
    arguments.push("--".to_owned());
    arguments.extend(options.compile.program_arguments.iter().cloned());
    arguments
}

fn read_hybrid_manifest(path: &Path) -> Result<HybridManifest, String> {
    let bytes = std::fs::read(path)
        .map_err(|error| format!("cannot read Hybrid manifest `{}`: {error}", path.display()))?;
    HybridManifest::from_bytes(&bytes).map_err(|error| {
        format!(
            "cannot decode Hybrid manifest `{}`: {error}",
            path.display()
        )
    })
}

fn breakpoint_function_name(requested: &str) -> &str {
    requested
        .rsplit_once(':')
        .filter(|(_, pc)| pc.parse::<usize>().is_ok())
        .map_or(requested, |(name, _)| name)
}

fn manifest_function<'a>(
    functions: &'a [HybridFunction],
    name: &str,
) -> Option<&'a HybridFunction> {
    functions.iter().find(|function| {
        function.name == name
            || function.exported_name.as_deref() == Some(name)
            || function.id.to_string() == name
    })
}

fn hybrid_breakpoints(
    manifest: &HybridManifest,
    options: &DebugOptions,
) -> Result<(Vec<String>, bool), String> {
    let mut native_symbols = Vec::new();
    let mut needs_vm_probe = false;
    if options.breakpoints.is_empty() {
        if let Some(entry) = manifest
            .entry
            .and_then(|id| manifest.functions.iter().find(|function| function.id == id))
        {
            match entry.execution {
                Execution::Native => {
                    if let Some(symbol) = entry.exported_name.as_deref() {
                        native_symbols.push(symbol.to_owned());
                    }
                }
                Execution::Runtime | Execution::Inherited => needs_vm_probe = true,
            }
        }
    } else {
        for requested in &options.breakpoints {
            let name = breakpoint_function_name(requested);
            let Some(function) = manifest_function(&manifest.functions, name) else {
                return Err(format!(
                    "no Hybrid function matches breakpoint `{requested}`"
                ));
            };
            match function.execution {
                Execution::Native => {
                    let Some(symbol) = function.exported_name.as_deref() else {
                        return Err(format!(
                            "Hybrid function `{name}` has no native breakpoint symbol"
                        ));
                    };
                    native_symbols.push(symbol.to_owned());
                }
                Execution::Runtime | Execution::Inherited => needs_vm_probe = true,
            }
        }
    }
    Ok((native_symbols, needs_vm_probe))
}

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
            let name = breakpoint_function_name(requested);
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
