//! The verbs that start a program: `check`, `run`, `debug`, `test`, and `live`.
//!
//! Each one resolves a path to a package, compiles it through the frontend
//! [`super`] owns, and hands the verified IR to the engine `--backend` selects.
//! What differs between them is only what they do with the program once it is
//! known good: report on it, run it, stop inside it, run its tests, or hold it
//! open and rebuild it on every save.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use kira_backend_api::BackendMode;
use kira_ir::IrProgram;
use kira_llvm_backend::NativeLinkInputs;
use kira_manifest::RunnerId;

use super::execute::{
    build_native, refuse_syscalls_on_the_vm, run_hybrid, run_native, run_on_vm, run_vm_module_file,
    run_web,
};
use super::{
    EXIT_FAILURE, EXIT_OK, EXIT_USAGE, apply_manifest_defaults, compile, emit_diagnostics,
    foreign_link, options_target, parse_options, resolve_foreign, resolve_path, runnable_ir,
    verified, verified_as,
};
use crate::debugger;
use crate::hybrid;
use crate::options::{CompileOptions, Device};
use crate::progress::{err, out};

/// Runs `kira check [file|dir] [--device ...] [--target ...]`: report
/// diagnostics, never execute.
///
/// With no path, checks the package you are standing in — the same default
/// `run` and `build` take.
///
/// The machine matters here even though nothing is emitted. Autobind generates
/// per target inside the frontend and native-library rows are selected per
/// target, so a program that checks clean for this machine can fail for another
/// one on a library it declares no row for — which is exactly the answer
/// `kira check --target aarch64-linux-gnu` is being asked for. This read the
/// manifest's own target and ignored the command line, so the two devices it
/// already accepted said nothing.
pub fn check(args: &[String]) -> i32 {
    let surface = crate::progress::Surface::install("Checking");
    let _guard = crate::progress::Finish(surface);
    // Parsed like every other compiling verb, so a flag they share means the
    // same thing here — `--timings` above all, which is asked of an analysis
    // more often than of a build.
    let options = match parse_options("check", args) {
        Ok(options) => options,
        Err(code) => return code,
    };
    crate::diagnostics::show_notes(options.show_notes);
    let _timings = crate::timings::Timings::install(options.timings);
    let path = options.path.as_str();
    match compile(path, &options_target(&options)) {
        Ok(compiled) => {
            emit_diagnostics(&compiled.diagnostics, &compiled.sources);
            if compiled.has_errors() {
                EXIT_FAILURE
            } else {
                out!("ok: {path}");
                EXIT_OK
            }
        }
        Err(code) => code,
    }
}

/// Runs `kira run [--backend vm|llvm|hybrid] [--device host|wasm32|wasm64]
/// <file|dir>`: report diagnostics, then execute a clean program.
///
/// A wasm device does not run on this machine: it builds a module and serves it
/// to a browser, which is what running a Kira program on the Web means.
pub fn run(args: &[String]) -> i32 {
    let surface = crate::progress::Surface::install("Running");
    let _guard = crate::progress::Finish(surface);
    let mut options = match parse_options("run", args) {
        Ok(options) => options,
        Err(code) => return code,
    };
    crate::diagnostics::show_notes(options.show_notes);
    let _timings = crate::timings::Timings::install(options.timings);
    options.path = match resolve_path(&options.path) {
        Ok(path) => path,
        Err(code) => return code,
    };
    let compiled = match verified(&options.path, &options_target(&options)) {
        Ok(compiled) => compiled,
        Err(code) => return code,
    };
    if let Err(code) = apply_manifest_defaults("run", &mut options, &compiled) {
        return code;
    }
    let ir = match runnable_ir("run", compiled) {
        Ok(ir) => ir,
        Err(code) => return code,
    };

    let foreign = match resolve_foreign(&options.path, &ir, &options.device) {
        Ok(foreign) => foreign,
        Err(code) => return code,
    };
    let link = foreign_link(&foreign);

    if let Some(device) = options.device.wasm() {
        return run_web(&ir, &options, device, link);
    }

    // A run started by `kira profile record` samples its own Kira call stack
    // while it runs and writes it where the recording will collect it. Every
    // other run starts no sampler and pays nothing: the interpreter selects a
    // dispatch loop without the publishing stores.
    let sampler = kira_profile::session::ChildSampler::start();
    // The native backend runs the program as a child and enforces the bound on
    // it there; ending this process would orphan the program instead.
    if let Some(bound) = options.quit_after
        && options.backend != BackendMode::LlvmNative
    {
        quit_after(bound);
    }
    let code = match options.backend {
        BackendMode::VmBytecode => run_on_vm(
            &ir,
            Path::new(&options.path),
            link,
            &options.program_arguments,
        ),
        BackendMode::LlvmNative => run_native(&ir, &options, link),
        BackendMode::Hybrid => run_hybrid(&ir, &options, link, &options.program_arguments),
    };
    if let Some(sampler) = sampler
        && let Err(error) = sampler.finish(&profile_symbols(&ir, &options))
    {
        err!("kira run: cannot write the profile samples: {error}");
    }
    code
}

/// Ends the process `bound` after the program starts.
///
/// The engines run the program in this process, and a program that owns a
/// window returns only when a person closes it. The deadline is therefore the
/// process's, not a callee's: nothing below has a loop to ask.
fn quit_after(bound: std::time::Duration) {
    std::thread::spawn(move || {
        std::thread::sleep(bound);
        crate::progress::out!("kira run: --quit-after reached");
        kira_toolchain::process::exit(EXIT_OK);
    });
}

/// The Kira identities a profiled run's samples are resolved against.
fn profile_symbols(ir: &IrProgram, options: &CompileOptions) -> kira_profile::symbols::KiraSymbols {
    let source = Path::new(&options.path);
    let info = kira_debug::DebugInfo::from_ir(
        ir,
        source
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_else(|| "program".to_owned()),
        debugger::backend(options.backend),
        Some(source),
    )
    .optimized(options.release);
    kira_profile::symbols::KiraSymbols::from_debug(&info)
}

/// Runs `kira debug [file|dir]` with a VM, hybrid, or LLVM/LLDB debugger.
pub fn debug(args: &[String]) -> i32 {
    let surface = crate::progress::Surface::install("Debugging");
    let _guard = crate::progress::Finish(surface);
    let mut debug_options = match debugger::parse(args) {
        Ok(options) => options,
        Err(error) => {
            err!("kira debug: {error}");
            return EXIT_USAGE;
        }
    };
    crate::diagnostics::show_notes(debug_options.compile.show_notes);
    let _timings = crate::timings::Timings::install(debug_options.compile.timings);
    debug_options.compile.path = match resolve_path(&debug_options.compile.path) {
        Ok(path) => path,
        Err(code) => return code,
    };
    let compiled = match verified(
        &debug_options.compile.path,
        &options_target(&debug_options.compile),
    ) {
        Ok(compiled) => compiled,
        Err(code) => return code,
    };
    if let Err(code) = apply_manifest_defaults("debug", &mut debug_options.compile, &compiled) {
        return code;
    }
    if !matches!(debug_options.compile.device, Device::Host) {
        err!("kira debug: the debugger currently targets host VM/LLVM/hybrid runs");
        return EXIT_USAGE;
    }
    let ir = match runnable_ir("debug", compiled) {
        Ok(ir) => ir,
        Err(code) => return code,
    };
    let source = Path::new(&debug_options.compile.path);
    let info = kira_debug::DebugInfo::from_ir(
        &ir,
        source
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_else(|| "program".to_owned()),
        debugger::backend(debug_options.compile.backend),
        Some(source),
    )
    .optimized(debug_options.compile.release);
    let foreign = match resolve_foreign(&debug_options.compile.path, &ir, &Device::Host) {
        Ok(foreign) => foreign,
        Err(code) => return code,
    };
    let link = foreign_link(&foreign);
    if debug_options.prepare {
        return debugger::prepare_target(&ir, source, link, &debug_options, &info);
    }
    match debug_options.compile.backend {
        BackendMode::VmBytecode => debugger::run_vm(&ir, source, link, &debug_options, &info),
        BackendMode::Hybrid => debugger::run_hybrid(&ir, source, link, &debug_options, &info),
        BackendMode::LlvmNative => debugger::run_llvm(&ir, source, link, &debug_options, &info),
    }
}

/// The function a `kira test` run enters.
///
/// Generated by a test package's `TestRunner` collector macro. The compiler
/// knows neither the `Test` family nor the protocol behind this entrypoint.
const TEST_ENTRY: &str = "kiraTestMain";
const TEST_NAME_PREFIX: &str = "__kira_test_name__:";
const TEST_RESULT_PREFIX: &str = "__kira_test_result__:";
const PREBUILT_VM_ENV: &str = "KIRA_TEST_PREBUILT_VM";
const PREBUILT_FFI_ENV: &str = "KIRA_TEST_PREBUILT_FFI";
const PREBUILT_NATIVE_ENV: &str = "KIRA_TEST_PREBUILT_NATIVE";
const PREBUILT_HYBRID_ENV: &str = "KIRA_TEST_PREBUILT_HYBRID";

/// Builds a program's tests and runs them, on the backend `--backend` selects.
///
/// The same pipeline `run` drives, with one difference: the entrypoint is the
/// generated `kiraTestMain` rather than `@Main`. A suite therefore needs no
/// `@Main` of its own, and one that has an application entrypoint keeps it —
/// `kira run` on the same package still runs that.
pub fn test(args: &[String]) -> i32 {
    let surface = crate::progress::Surface::install("Testing");
    let _guard = crate::progress::Finish(surface);
    let mut options = match parse_options("test", args) {
        Ok(options) => options,
        Err(code) => return code,
    };
    crate::diagnostics::show_notes(options.show_notes);
    let _timings = crate::timings::Timings::install(options.timings);
    if let Some(code) = run_prebuilt_test(&options) {
        return code;
    }
    options.path = match resolve_path(&options.path) {
        Ok(path) => path,
        Err(code) => return code,
    };
    // Compiled as a test run, which neither requires an `@Main` nor refuses
    // one: a suite is entered through the generated runner, and a package that
    // is both an application and a suite keeps both entrypoints.
    let compiled = match verified_as(
        "test",
        &options.path,
        kira_semantics::BuildKind::Test,
        &options_target(&options),
    ) {
        Ok(compiled) => compiled,
        Err(code) => return code,
    };
    if let Err(code) = apply_manifest_defaults("test", &mut options, &compiled) {
        return code;
    }
    let mut ir = compiled.ir;
    // The entrypoint is retargeted before anything reads it, so every backend
    // sees an ordinary program whose `main` happens to be the runner.
    match ir
        .functions
        .iter()
        .position(|function| function.name == TEST_ENTRY)
    {
        Some(index) => ir.main = Some(index as u32),
        None => {
            err!(
                "kira test: this program has no tests to run\n\
                 note: a test package supplies the `Test` declaration family and runner"
            );
            return EXIT_FAILURE;
        }
    }

    let foreign = match resolve_foreign(&options.path, &ir, &options.device) {
        Ok(foreign) => foreign,
        Err(code) => return code,
    };
    let link = foreign_link(&foreign);

    if !options.program_arguments.is_empty() {
        return match options.backend {
            BackendMode::VmBytecode => run_on_vm(
                &ir,
                Path::new(&options.path),
                link,
                &options.program_arguments,
            ),
            BackendMode::LlvmNative => run_native(&ir, &options, link),
            BackendMode::Hybrid => run_hybrid(&ir, &options, link, &options.program_arguments),
        };
    }

    // Driving a test suite means starting the program and reading what it
    // prints, which needs a machine that can start it. A Web device has no
    // process to start, and a cross target's binary is one this machine will not
    // load — both are refused here rather than at whatever the operating system
    // says about a file it declined to execute.
    if !matches!(options.device, Device::Host) {
        err!(
            "kira test: test driving needs a program this machine can start, and \
             `{}` is not this machine",
            options.device
        );
        return EXIT_FAILURE;
    }

    let artifact = match build_test_artifact(&ir, &options, link) {
        Ok(artifact) => artifact,
        Err(code) => return code,
    };
    run_test_suite(&options, &artifact)
}

enum TestArtifact {
    Vm {
        module: PathBuf,
        binding_paths: Option<PathBuf>,
    },
    Native {
        executable: PathBuf,
    },
    Hybrid {
        manifest: PathBuf,
    },
}

impl Drop for TestArtifact {
    fn drop(&mut self) {
        if let Self::Vm {
            module,
            binding_paths,
        } = self
        {
            let _ = std::fs::remove_file(module);
            if let Some(path) = binding_paths {
                let _ = std::fs::remove_file(path);
            }
        }
    }
}

fn build_test_artifact(
    ir: &IrProgram,
    options: &CompileOptions,
    foreign_link: &NativeLinkInputs,
) -> Result<TestArtifact, i32> {
    match options.backend {
        BackendMode::VmBytecode => {
            // The same gate `run` applies, and for the same reason: a suite that
            // names a call the interpreter will not serve has to hear so before
            // any case runs, not as a trap in the middle of the report.
            if refuse_syscalls_on_the_vm(ir) {
                return Err(EXIT_USAGE);
            }
            let module = match kira_bytecode::compile(ir) {
                Ok(module) => module,
                Err(error) => {
                    err!("kira: bytecode compilation failed: {error}");
                    return Err(EXIT_FAILURE);
                }
            };
            let binding_paths = if ir.foreign_imports.is_empty() && ir.foreign_callbacks.is_empty()
            {
                None
            } else {
                let bindings = match crate::native::direct_foreign_bindings(
                    ir,
                    Path::new(&options.path),
                    foreign_link,
                ) {
                    Ok(bindings) => bindings,
                    Err(error) => {
                        err!("kira: {error}");
                        return Err(EXIT_FAILURE);
                    }
                };
                let path =
                    std::env::temp_dir().join(format!("kira-test-{}-ffi.txt", std::process::id()));
                if let Err(error) = crate::native::write_foreign_binding_paths(&path, &bindings) {
                    err!("kira: {error}");
                    return Err(EXIT_FAILURE);
                }
                Some(path)
            };
            let module_path =
                std::env::temp_dir().join(format!("kira-test-{}-module.kbc", std::process::id()));
            if let Err(error) = std::fs::write(&module_path, module.to_bytes()) {
                err!(
                    "kira test: cannot write `{}`: {error}",
                    module_path.display()
                );
                return Err(EXIT_FAILURE);
            }
            Ok(TestArtifact::Vm {
                module: module_path,
                binding_paths,
            })
        }
        BackendMode::LlvmNative => {
            let Some(artifacts) = build_native(ir, options, foreign_link) else {
                return Err(EXIT_FAILURE);
            };
            let Some(executable) = artifacts.executable else {
                err!("kira test: the native build produced no executable");
                return Err(EXIT_FAILURE);
            };
            Ok(TestArtifact::Native { executable })
        }
        BackendMode::Hybrid => match hybrid::build(
            ir,
            Path::new(&options.path),
            options.emit_llvm_ir,
            foreign_link,
        ) {
            Ok(bundle) => Ok(TestArtifact::Hybrid {
                manifest: bundle.manifest,
            }),
            Err(error) => {
                err!("kira: {error}");
                Err(EXIT_FAILURE)
            }
        },
    }
}

fn run_prebuilt_test(options: &CompileOptions) -> Option<i32> {
    if options.program_arguments.is_empty() {
        return None;
    }
    match options.backend {
        BackendMode::VmBytecode => {
            let module = std::env::var_os(PREBUILT_VM_ENV).map(PathBuf::from)?;
            let binding_paths = std::env::var_os(PREBUILT_FFI_ENV).map(PathBuf::from);
            Some(run_vm_module_file(
                &module,
                binding_paths.as_deref(),
                &options.program_arguments,
            ))
        }
        BackendMode::LlvmNative => {
            let executable = std::env::var_os(PREBUILT_NATIVE_ENV).map(PathBuf::from)?;
            Some(
                match crate::native::execute(
                    &executable,
                    &options.program_arguments,
                    options.quit_after,
                ) {
                    Ok(code) => code,
                    Err(error) => {
                        err!("kira: {error}");
                        EXIT_FAILURE
                    }
                },
            )
        }
        BackendMode::Hybrid => {
            let manifest = std::env::var_os(PREBUILT_HYBRID_ENV).map(PathBuf::from)?;
            Some(
                match hybrid::run_bundle(&manifest, &options.program_arguments) {
                    Ok(code) => code,
                    Err(error) => {
                        err!("kira: {error}");
                        EXIT_FAILURE
                    }
                },
            )
        }
    }
}

fn run_test_suite(options: &CompileOptions, artifact: &TestArtifact) -> i32 {
    let names = match enumerate_tests(options, artifact) {
        Ok(names) => names,
        Err(code) => return code,
    };
    let mut passed = 0usize;
    let mut failed = 0usize;
    let mut skipped = 0usize;
    for name in &names {
        let outcome = match run_test_case(options, artifact, name) {
            Ok(outcome) => outcome,
            Err(code) => return code,
        };
        match outcome {
            TestCaseOutcome::Passed => {
                passed += 1;
                out!("ok   {name}");
            }
            TestCaseOutcome::Failed => {
                failed += 1;
                out!("FAIL {name}");
            }
            TestCaseOutcome::Skipped => {
                skipped += 1;
                out!("skip {name} (diagnostic/compile expectation unsupported)");
            }
        }
    }
    out!(
        "{passed} passed, {failed} failed, {skipped} skipped, {} total",
        names.len()
    );
    if failed == 0 { EXIT_OK } else { EXIT_FAILURE }
}

enum TestCaseOutcome {
    Passed,
    Failed,
    Skipped,
}

fn enumerate_tests(options: &CompileOptions, artifact: &TestArtifact) -> Result<Vec<String>, i32> {
    let output = run_test_child(options, artifact, "list", None)?;
    if !output.status.success() {
        report_child_failure("test enumeration", &output);
        return Err(EXIT_FAILURE);
    }
    if protocol_result(&output.stdout) != Some(EXIT_OK) {
        report_child_failure("test enumeration protocol", &output);
        return Err(EXIT_FAILURE);
    }
    let names = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| line.strip_prefix(TEST_NAME_PREFIX))
        .filter(|name| !name.is_empty())
        .map(str::to_owned)
        .collect();
    Ok(names)
}

fn run_test_case(
    options: &CompileOptions,
    artifact: &TestArtifact,
    name: &str,
) -> Result<TestCaseOutcome, i32> {
    let expectation = run_test_child(options, artifact, "expect", Some(name))?;
    match action_code(&expectation) {
        Some(10) => {
            let check = run_test_child(options, artifact, "check", Some(name))?;
            Ok(match action_code(&check) {
                Some(0) => TestCaseOutcome::Passed,
                _ => TestCaseOutcome::Failed,
            })
        }
        Some(11) => {
            let body = run_test_child(options, artifact, "body", Some(name))?;
            Ok(if body.status.code() == Some(EXIT_FAILURE) {
                TestCaseOutcome::Passed
            } else {
                TestCaseOutcome::Failed
            })
        }
        Some(12) => Ok(TestCaseOutcome::Skipped),
        _ => Ok(TestCaseOutcome::Failed),
    }
}

fn run_test_child(
    options: &CompileOptions,
    artifact: &TestArtifact,
    action: &str,
    name: Option<&str>,
) -> Result<Output, i32> {
    let executable = std::env::current_exe().map_err(|error| {
        err!("kira test: cannot locate the CLI executable: {error}");
        EXIT_FAILURE
    })?;
    let mut command = Command::new(executable);
    command
        .arg("test")
        .arg("--backend")
        .arg(options.backend.label());
    if options.release {
        command.arg("--release");
    }
    if options.emit_llvm_ir {
        command.arg("--emit-llvm-ir");
    }
    command.arg(&options.path).arg("--").arg(action);
    if let Some(name) = name {
        command.arg(name);
    }
    for variable in [
        PREBUILT_VM_ENV,
        PREBUILT_FFI_ENV,
        PREBUILT_NATIVE_ENV,
        PREBUILT_HYBRID_ENV,
    ] {
        command.env_remove(variable);
    }
    match artifact {
        TestArtifact::Vm {
            module,
            binding_paths,
        } => {
            command.env(PREBUILT_VM_ENV, module);
            if let Some(binding_paths) = binding_paths {
                command.env(PREBUILT_FFI_ENV, binding_paths);
            }
        }
        TestArtifact::Native { executable } => {
            command.env(PREBUILT_NATIVE_ENV, executable);
        }
        TestArtifact::Hybrid { manifest } => {
            command.env(PREBUILT_HYBRID_ENV, manifest);
        }
    }
    command.output().map_err(|error| {
        err!("kira test: cannot start the {action} child: {error}");
        EXIT_FAILURE
    })
}

fn action_code(output: &Output) -> Option<i32> {
    match output.status.code() {
        Some(code) if code != EXIT_OK => Some(code),
        Some(_) => protocol_result(&output.stdout).or(Some(EXIT_OK)),
        None => protocol_result(&output.stdout),
    }
}

fn protocol_result(stdout: &[u8]) -> Option<i32> {
    String::from_utf8_lossy(stdout)
        .lines()
        .rev()
        .find_map(|line| {
            line.strip_prefix(TEST_RESULT_PREFIX)
                .and_then(|code| code.parse().ok())
        })
}

fn report_child_failure(label: &str, output: &Output) {
    let stderr = String::from_utf8_lossy(&output.stderr);
    if stderr.trim().is_empty() {
        err!("kira test: {label} child exited {:?}", output.status.code());
    } else {
        err!("kira test: {label} failed:\n{stderr}");
    }
}

/// Runs `kira live [runner] [file|dir] [--backend vm|llvm|hybrid]`: build a bundle,
/// serve it, and run it on a runner client.
///
/// Unlike `run`, this does not execute the program in this process: it builds a
/// `.klbundle`, serves it over a socket, and a runner client runs it. That is
/// what makes a live session a live session rather than a run — the app is
/// hosted somewhere that can outlive the compiler and take a new bundle later.
///
/// With no path, this is the package you are standing in — the same default
/// `run`, `build`, and `check` take.
pub fn live(args: &[String]) -> i32 {
    let options = match crate::live::LiveOptions::parse(args) {
        Ok(options) => options,
        Err(error) => {
            err!("kira live: {error}");
            return EXIT_USAGE;
        }
    };
    // Each runner family owns its session shape: the desktop hosts in a spawned
    // client, an exported Xcode app builds and launches itself, the Web serves
    // a page, and scaffolds are audited where no loop exists yet.
    match options.runner {
        RunnerId::Macos | RunnerId::Ios | RunnerId::Tvos | RunnerId::Visionos => {
            return crate::live_apple::run(&options);
        }
        RunnerId::Web => return crate::live_web::run(&options),
        RunnerId::Windows | RunnerId::Linux => return crate::live_scaffold::run(&options),
        RunnerId::Android => {
            err!(
                "kira live: the `android` runner has no runner client in this build yet; \
                 export the Gradle project with `kira export android` and install it by hand"
            );
            return EXIT_FAILURE;
        }
        RunnerId::Desktop => {}
    }
    // Two paths, because a package is a tree and a build has one entry. The
    // watched path is what the user named — for a package, the whole directory,
    // so a save anywhere in `app/` reloads. The source is the entry package
    // discovery resolves it to, which is what names the build artifacts.
    let watched = PathBuf::from(&options.path);
    let entry = match resolve_path(&options.path) {
        Ok(entry) => entry,
        Err(code) => return code,
    };
    let source = Path::new(&entry);

    // Compiling is a closure rather than a value, because a watched session
    // rebuilds: the frontend runs again for every save, and a save that does not
    // compile yields `None` rather than an error, so the session keeps the app
    // that is already running. The frontend itself stays alive in this closure;
    // its Salsa queries are deliberately incremental across these calls.
    let mut frontend = kira_build::FrontendSession::new();
    let mut rebuild = || -> Result<Option<crate::supervisor::LiveBuild>, crate::live::LiveError> {
        let Ok(compiled) = crate::pipeline::runnable_path_compiled_with_frontend(
            &entry,
            &crate::foreign_libs::target_for_device(&Device::Host),
            &mut frontend,
        ) else {
            return Ok(None);
        };
        let watch_set = crate::live::watch_set(&watched, &compiled.sources);
        let Ok(ir) = runnable_ir("live", compiled) else {
            return Ok(None);
        };
        // A live session runs on the machine the runner runs on, so the foreign
        // libraries it links are the host's — resolved on every rebuild, because
        // a save can add an import that needs one. A resolution failure is a
        // failed build like any other: the session keeps the app it is running
        // and the diagnostic is already on stderr.
        let Ok(foreign) = resolve_foreign(&entry, &ir, &Device::Host) else {
            return Ok(None);
        };
        let bundle = crate::live::build_bundle(
            &ir,
            source,
            options.runner,
            options.backend,
            foreign_link(&foreign),
        )?;
        Ok(Some(crate::supervisor::LiveBuild { bundle, watch_set }))
    };

    match crate::supervisor::run_desktop(&options, &mut rebuild) {
        Ok(()) => EXIT_OK,
        Err(error) => {
            err!("kira live: {error}");
            EXIT_FAILURE
        }
    }
}
