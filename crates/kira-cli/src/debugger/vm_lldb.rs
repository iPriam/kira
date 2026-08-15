//! Real LLDB hosting for the bytecode VM.
//!
//! LLDB cannot decode Kira bytecode as machine instructions. The VM therefore
//! exposes one stable native probe frame per instruction and keeps the actual
//! bytecode/module state in the interpreter. A conditional breakpoint on that
//! frame gives LLDB control over VM function/PC locations while the probe's
//! exported C-shaped state exposes decoded locals, the operand stack, and the
//! VM backtrace for native inspection.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use kira_bytecode::Module;
use kira_debug::{
    Breakpoint, DebugInfo, LldbDapBreakpoint, LldbDapLaunch, LldbLaunch, VM_PROBE_SYMBOL,
    VM_TEXT_SYMBOL, VmProbe,
};
use kira_ir::IrProgram;
use kira_llvm_backend::NativeLinkInputs;
use kira_main::{ForeignSession, StdoutHost};
use kira_runtime_abi::{NativeStateHost, env};
use kira_vm_runtime::{VmLldbBreakpoint, VmLldbObserver};

use super::DebugOptions;
use crate::native;
use crate::pipeline::{EXIT_FAILURE, EXIT_OK};
use crate::progress::{err, out};

/// Reads the decoded stop state from the process, without calling into it.
fn vm_text_command() -> String {
    format!("memory read --format c --size 1 --count 512 &{VM_TEXT_SYMBOL}")
}

/// Runs a compiled VM module under a real LLDB process.
pub(super) fn run_under_lldb(
    ir: &IrProgram,
    module: &Module,
    source: &Path,
    foreign_link: &NativeLinkInputs,
    options: &DebugOptions,
    info: &DebugInfo,
) -> i32 {
    let module_path = temporary_module_path(source);
    if let Err(error) = std::fs::write(&module_path, module.to_bytes()) {
        err!(
            "kira debug: cannot write LLDB VM module `{}`: {error}",
            module_path.display()
        );
        return EXIT_FAILURE;
    }

    let binding_paths = match prepare_binding_paths(ir, source, foreign_link) {
        Ok(path) => path,
        Err(error) => {
            let _ = std::fs::remove_file(&module_path);
            err!("kira: {error}");
            return EXIT_FAILURE;
        }
    };
    let target = match std::env::current_exe() {
        Ok(target) => target,
        Err(error) => {
            let _ = std::fs::remove_file(&module_path);
            err!("kira debug: cannot locate the LLDB VM host executable: {error}");
            return EXIT_FAILURE;
        }
    };
    let condition = match vm_breakpoint_condition(&options.breakpoints, info) {
        Ok(condition) => condition,
        Err(error) => {
            let _ = std::fs::remove_file(&module_path);
            err!("kira debug: {error}");
            return EXIT_FAILURE;
        }
    };
    let mut launch = LldbLaunch::from_info(&target, info);
    launch.breakpoints.clear();
    if let Some(condition) = condition {
        launch.add_conditional_breakpoint(VM_PROBE_SYMBOL, condition);
    } else {
        launch.add_breakpoint(VM_PROBE_SYMBOL);
    }
    // Read the mirror from the stopped process instead of calling a target
    // function. Swift LLDB's Windows expression evaluator can crash when a
    // Rust printing function is invoked on repeated interactive stops, so the
    // automatic text command is batch-only; interactive users can issue raw
    // memory-read commands as often as they like.
    if options.batch {
        launch.add_breakpoint_command(1, vm_text_command());
    }
    launch.disassemble = options.disassemble;
    launch.batch = options.batch;
    // The Windows Swift LLDB build aborts while unwinding the Rust VM probe
    // frame, just as it does for hybrid DLL frames. Register inspection and
    // disassembly remain valid at the probe entry, so omit only this query.
    launch.thread_backtrace = false;
    launch.arguments = vm_host_arguments(&module_path, binding_paths.as_deref(), options);
    print_vm_source_context(source, info, module.main, &options.breakpoints);
    out!("LLDB VM host: {}", target.display());
    out!("LLDB VM probe: {VM_PROBE_SYMBOL}");

    let result = match launch.launch() {
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
    };
    let _ = std::fs::remove_file(&module_path);
    result
}

/// Runs a VM module through LLDB's DAP frontend.
///
/// Unlike the command interpreter, DAP keeps the target stopped across
/// repeated `continue` requests and reads the VM state through the debugger's
/// `evaluate`/`readMemory` requests. This is the reliable multi-stop path for
/// Windows Swift LLDB while remaining a real LLDB-owned process.
pub(super) fn run_under_lldb_dap(
    ir: &IrProgram,
    module: &Module,
    source: &Path,
    foreign_link: &NativeLinkInputs,
    options: &DebugOptions,
    info: &DebugInfo,
) -> i32 {
    let module_path = temporary_module_path(source);
    if let Err(error) = std::fs::write(&module_path, module.to_bytes()) {
        err!(
            "kira debug: cannot write LLDB DAP VM module `{}`: {error}",
            module_path.display()
        );
        return EXIT_FAILURE;
    }
    let binding_paths = match prepare_binding_paths(ir, source, foreign_link) {
        Ok(path) => path,
        Err(error) => {
            let _ = std::fs::remove_file(&module_path);
            err!("kira: {error}");
            return EXIT_FAILURE;
        }
    };
    let target = match std::env::current_exe() {
        Ok(target) => target,
        Err(error) => {
            let _ = std::fs::remove_file(&module_path);
            err!("kira debug: cannot locate the LLDB DAP VM host executable: {error}");
            return EXIT_FAILURE;
        }
    };
    if let Err(error) = vm_breakpoint_condition(&options.breakpoints, info) {
        let _ = std::fs::remove_file(&module_path);
        err!("kira debug: {error}");
        return EXIT_FAILURE;
    }
    let mut launch = LldbDapLaunch::new(&target);
    // The VM host itself waits for the requested Kira function/PC before it
    // calls the probe. Keep the native DAP breakpoint unconditional so every
    // later `continue` reaches the next VM instruction as well.
    launch.add_breakpoint(LldbDapBreakpoint::new(VM_PROBE_SYMBOL));
    launch.set_text_symbol(VM_TEXT_SYMBOL);
    launch.set_disassemble(options.disassemble);
    launch.set_continue_count(options.dap_continues);
    launch.arguments = vm_host_arguments(&module_path, binding_paths.as_deref(), options);
    print_vm_source_context(source, info, module.main, &options.breakpoints);
    out!("LLDB DAP VM host: {}", target.display());
    let result = match launch.launch() {
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
    };
    let _ = std::fs::remove_file(&module_path);
    result
}

/// Runs the private VM host command launched by LLDB.
pub(crate) fn run_host(args: &[String]) -> i32 {
    let options = match parse_host_args(args) {
        Ok(options) => options,
        Err(error) => {
            err!("kira: {error}");
            return EXIT_FAILURE;
        }
    };
    let bytes = match std::fs::read(&options.module) {
        Ok(bytes) => bytes,
        Err(error) => {
            err!(
                "kira: cannot read LLDB VM module `{}`: {error}",
                options.module.display()
            );
            return EXIT_FAILURE;
        }
    };
    let module = match Module::from_bytes(&bytes) {
        Ok(module) => module,
        Err(error) => {
            err!("kira: cannot decode LLDB VM module: {error}");
            return EXIT_FAILURE;
        }
    };
    let breakpoints = match vm_lldb_breakpoints(&options.breakpoints, &module) {
        Ok(breakpoints) => breakpoints,
        Err(error) => {
            err!("kira: {error}");
            return EXIT_FAILURE;
        }
    };
    let mut observer = VmLldbObserver::with_breakpoints(breakpoints);
    let result = if module.foreign_imports.is_empty() && module.foreign_callbacks.is_empty() {
        run_without_bindings(module, &options.program_arguments, &mut observer)
    } else {
        run_with_bindings(
            module,
            options.binding_paths.as_deref(),
            &options.program_arguments,
            &mut observer,
        )
    };
    match result {
        Ok(()) => EXIT_OK,
        Err(error) => {
            err!("kira: runtime trap: {error}");
            EXIT_FAILURE
        }
    }
}

fn run_without_bindings(
    module: Module,
    arguments: &[String],
    observer: &mut VmLldbObserver,
) -> Result<(), String> {
    let mut host = NativeStateHost::new(StdoutHost);
    // SAFETY: this private LLDB host owns the process-environment boundary and
    // does not access it from another thread while the VM executes.
    unsafe {
        env::with_arguments(arguments, || {
            kira_vm_runtime::execute_with_debug(&module, &mut host, observer).map(|_| ())
        })
    }
    .map_err(|error| error.to_string())
}

fn run_with_bindings(
    module: Module,
    binding_paths: Option<&Path>,
    arguments: &[String],
    observer: &mut VmLldbObserver,
) -> Result<(), String> {
    let paths = binding_paths
        .map(native::read_foreign_binding_paths)
        .transpose()
        .map_err(|error| error.to_string())?
        .unwrap_or_else(|| vec![None; module.foreign_imports.len()]);
    if paths.len() != module.foreign_imports.len() {
        return Err(format!(
            "foreign binding manifest has {} paths for {} imports",
            paths.len(),
            module.foreign_imports.len()
        ));
    }
    let imports = module
        .foreign_imports
        .iter()
        .zip(paths)
        .map(|(entry, path)| {
            path.map_or_else(
                || kira_main::ForeignBinding::unavailable(entry.signature().clone()),
                |path| {
                    if native::is_process_binding_path(&path) {
                        kira_main::ForeignBinding::process(
                            entry.symbol(),
                            entry.signature().clone(),
                        )
                    } else {
                        kira_main::ForeignBinding::dynamic(
                            path,
                            entry.symbol(),
                            entry.signature().clone(),
                        )
                    }
                },
            )
        })
        .collect();
    let callbacks = module
        .foreign_callbacks
        .iter()
        .map(|callback| callback.signature().clone())
        .collect();
    let aggregates = module.foreign_aggregates.clone();
    let program = kira_vm_runtime::Program::load(module).map_err(|error| error.to_string())?;
    let session = ForeignSession::load_dynamic(program, imports, callbacks, aggregates)
        .map_err(|error| error.to_string())?;
    // SAFETY: this private LLDB host owns the process-environment boundary and
    // does not access it from another thread while the VM executes.
    unsafe { env::with_arguments(arguments, || session.run_with_debug(observer).map(|_| ())) }
        .map_err(|error| error.to_string())
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct VmHostOptions {
    module: PathBuf,
    binding_paths: Option<PathBuf>,
    breakpoints: Vec<String>,
    program_arguments: Vec<String>,
}

fn parse_host_args(args: &[String]) -> Result<VmHostOptions, String> {
    let mut module = None;
    let mut binding_paths = None;
    let mut breakpoints = Vec::new();
    let mut program_arguments = Vec::new();
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--module" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| "`--module` expects a path".to_owned())?;
                module = Some(PathBuf::from(value));
                index += 1;
            }
            "--bindings" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| "`--bindings` expects a path".to_owned())?;
                binding_paths = Some(PathBuf::from(value));
                index += 1;
            }
            "--vm-break" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| "`--vm-break` expects a function or function:pc".to_owned())?;
                breakpoints.push(value.clone());
                index += 1;
            }
            "--" => {
                program_arguments.extend(args[index + 1..].iter().cloned());
                break;
            }
            other => return Err(format!("unknown LLDB VM host argument `{other}`")),
        }
        index += 1;
    }
    Ok(VmHostOptions {
        module: module.ok_or_else(|| "LLDB VM host needs `--module`".to_owned())?,
        binding_paths,
        breakpoints,
        program_arguments,
    })
}

pub(super) fn vm_host_arguments(
    module: &Path,
    binding_paths: Option<&Path>,
    options: &DebugOptions,
) -> Vec<String> {
    let mut arguments = vec![
        super::VM_DEBUG_HOST.to_owned(),
        "--module".to_owned(),
        module.display().to_string(),
    ];
    if let Some(binding_paths) = binding_paths {
        arguments.push("--bindings".to_owned());
        arguments.push(binding_paths.display().to_string());
    }
    for breakpoint in &options.breakpoints {
        arguments.push("--vm-break".to_owned());
        arguments.push(breakpoint.clone());
    }
    arguments.push("--".to_owned());
    arguments.extend(options.compile.program_arguments.iter().cloned());
    arguments
}

fn vm_lldb_breakpoints(
    requested: &[String],
    module: &Module,
) -> Result<Vec<VmLldbBreakpoint>, String> {
    let functions = module
        .functions
        .iter()
        .enumerate()
        .map(|(id, function)| (id as u32, function.name.as_str()))
        .collect::<Vec<_>>();
    probe_breakpoints(requested, &functions)
}

/// Resolves breakpoint spellings against a function table for the VM probe.
///
/// Shared by both hosts: the VM's table comes from its bytecode module and the
/// hybrid host's from its manifest, but a breakpoint means the same thing to
/// the probe either way.
pub(super) fn probe_breakpoints(
    requested: &[String],
    functions: &[(u32, &str)],
) -> Result<Vec<VmLldbBreakpoint>, String> {
    requested
        .iter()
        .map(|value| {
            let breakpoint =
                Breakpoint::parse(value).ok_or_else(|| format!("invalid breakpoint `{value}`"))?;
            let function = functions.iter().find(|(id, name)| {
                *name == breakpoint.function_name || id.to_string() == breakpoint.function_name
            });
            let Some((function_id, _)) = function else {
                return Err(format!("no VM function matches breakpoint `{value}`"));
            };
            let pc = breakpoint.pc.unwrap_or(0);
            Ok(VmLldbBreakpoint {
                function_id: *function_id,
                pc: u32::try_from(pc).map_err(|_| {
                    format!("VM breakpoint `{value}` has an instruction index too large")
                })?,
            })
        })
        .collect()
}

fn vm_breakpoint_condition(
    requested: &[String],
    info: &DebugInfo,
) -> Result<Option<String>, String> {
    if requested.is_empty() {
        return Ok(None);
    }
    let probe = VmProbe::host();
    let mut conditions = Vec::with_capacity(requested.len());
    for value in requested {
        let breakpoint =
            Breakpoint::parse(value).ok_or_else(|| format!("invalid breakpoint `{value}`"))?;
        let function = info.functions.iter().find(|function| {
            function.name == breakpoint.function_name
                || function.id.to_string() == breakpoint.function_name
        });
        let Some(function) = function else {
            return Err(format!("no VM function matches breakpoint `{value}`"));
        };
        let pc = breakpoint.pc.unwrap_or(0);
        let pc = u32::try_from(pc)
            .map_err(|_| format!("VM breakpoint `{value}` has an instruction index too large"))?;
        let Some(condition) = probe.condition(function.id, pc) else {
            return Ok(None);
        };
        conditions.push(condition);
    }
    Ok(Some(conditions.join(" || ")))
}

fn print_vm_source_context(
    source: &Path,
    info: &DebugInfo,
    main: Option<u32>,
    requested: &[String],
) {
    let Ok(text) = std::fs::read_to_string(source) else {
        return;
    };
    let lines = text.lines().collect::<Vec<_>>();
    let mut ids = Vec::new();
    if requested.is_empty() {
        if let Some(main) = main {
            ids.push(main);
        }
    } else {
        for value in requested {
            if let Some(breakpoint) = Breakpoint::parse(value)
                && let Some(function) = info.functions.iter().find(|function| {
                    function.name == breakpoint.function_name
                        || function.id.to_string() == breakpoint.function_name
                })
            {
                ids.push(function.id);
            }
        }
    }
    ids.sort_unstable();
    ids.dedup();
    for function in info
        .functions
        .iter()
        .filter(|function| ids.contains(&function.id))
    {
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

pub(super) fn temporary_module_path(source: &Path) -> PathBuf {
    let parent = source.parent().unwrap_or_else(|| Path::new("."));
    let stem = source.file_stem().map_or_else(
        || "program".to_owned(),
        |stem| stem.to_string_lossy().into_owned(),
    );
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    parent.join(format!(
        ".kira-vm-debug-{stem}-{}-{nanos}.kbc",
        std::process::id()
    ))
}

fn prepare_binding_paths(
    ir: &IrProgram,
    source: &Path,
    foreign_link: &NativeLinkInputs,
) -> Result<Option<PathBuf>, String> {
    if ir.foreign_imports.is_empty() {
        return Ok(None);
    }
    let bindings = native::direct_foreign_bindings(ir, source, foreign_link)
        .map_err(|error| error.to_string())?;
    let path = temporary_binding_path(source);
    native::write_foreign_binding_paths(&path, &bindings).map_err(|error| error.to_string())?;
    Ok(Some(path))
}

pub(super) fn temporary_binding_path(source: &Path) -> PathBuf {
    let parent = source.parent().unwrap_or_else(|| Path::new("."));
    let stem = source.file_stem().map_or_else(
        || "program".to_owned(),
        |stem| stem.to_string_lossy().into_owned(),
    );
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    parent.join(format!(
        ".kira-vm-debug-{stem}-{}-{nanos}.ffi",
        std::process::id()
    ))
}
