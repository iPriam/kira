//! Executing a compiled program, and the native build a run consumes.
//!
//! The back half of the `run` pipeline: once [`super`] has a verified
//! [`IrProgram`], these routines run it on whichever backend was selected —
//! the VM, a native executable, the hybrid host, or a Web device — and forward
//! the program's exit status. [`build_native`] is here too, because a native
//! run and a native `build` produce the artifact the same way.

use kira_backend_api::WasmDevice;
use kira_ir::IrProgram;
use kira_llvm_backend::NativeLinkInputs;
use kira_main::StdoutHost;
use kira_runtime_abi::{NativeStateHost, env};

use super::{EXIT_FAILURE, EXIT_OK, EXIT_USAGE};
use crate::options::CompileOptions;
use crate::progress::err;
use crate::{hybrid, native, wasm};

/// Builds a program for the Web and serves it, opening a browser at it.
pub(super) fn run_web(
    ir: &IrProgram,
    options: &CompileOptions,
    device: WasmDevice,
    foreign_link: &NativeLinkInputs,
) -> i32 {
    match wasm::run(
        ir,
        std::path::Path::new(&options.path),
        device,
        foreign_link,
    ) {
        Ok(()) => EXIT_OK,
        Err(error) => {
            err!("kira: {error}");
            EXIT_FAILURE
        }
    }
}

/// Refuses the pure interpreter a program that enters the kernel, naming the
/// calls, and answers whether it did.
///
/// The VM is the one engine with nowhere to put the instruction. A `@FFI.Syscall`
/// *is* an instruction — `svc #0`, `syscall` — and a bytecode module carries no
/// instruction stream of its own: what it carries has to load on every machine
/// Kira runs on, including a `wasm32` one where there is no kernel entry sequence
/// at all.
///
/// Hybrid is not refused, and the difference is real rather than a concession. Its
/// native half is compiled through the same LLVM backend a `--backend llvm` build
/// is, so a bodyless declaration's body is machine code there exactly as it is in
/// an executable — which is already how hybrid calls an `@FFI.Extern`. The
/// bytecode half reaches it the way it reaches any other native function.
///
/// Refused here, before the program starts, for the same reason `run` refuses a
/// cross target by name: the engine is chosen on the command line, so the fix is
/// on the command line, and a program that has already begun writing output is
/// past the point where that can be said.
fn refuse_syscalls_on_the_vm(ir: &IrProgram) -> bool {
    let named: Vec<&str> = ir
        .foreign_imports
        .iter()
        .filter(|entry| entry.import.as_syscall().is_some())
        .map(|entry| entry.import.symbol())
        .collect();
    if named.is_empty() {
        return false;
    }
    err!(
        "kira: this program calls the Linux kernel directly ({}), which the VM engine cannot do: a \
         system call is an instruction, and the interpreter has no instruction stream of its own \
         to put one in. Run it with `--backend llvm` or `--backend hybrid`, both of which emit the \
         kernel entry sequence for the machine they are building for.",
        named.join(", ")
    );
    true
}

/// Builds a hybrid bundle and runs it in the hybrid host.
///
/// The host runs in this process rather than as a child: the native half is a
/// library, not an executable, and this process is what loads it.
pub(super) fn run_hybrid(
    ir: &IrProgram,
    options: &CompileOptions,
    foreign_link: &NativeLinkInputs,
    program_arguments: &[String],
) -> i32 {
    match hybrid::run(
        ir,
        std::path::Path::new(&options.path),
        options.emit_llvm_ir,
        foreign_link,
        program_arguments,
    ) {
        Ok(code) => code,
        Err(error) => {
            err!("kira: {error}");
            EXIT_FAILURE
        }
    }
}

/// Compiles the IR to bytecode and runs it on the VM.
///
/// A program with foreign imports opens its declared libraries and routes every
/// call and callback through the bundled Libffi host. A program with no foreign
/// surface runs against the plain stdout host.
pub(super) fn run_on_vm(
    ir: &IrProgram,
    source: &std::path::Path,
    foreign_link: &NativeLinkInputs,
    program_arguments: &[String],
) -> i32 {
    if refuse_syscalls_on_the_vm(ir) {
        return EXIT_USAGE;
    }
    kira_diagnostics::progress!("compiling bytecode");
    let module = match kira_bytecode::compile(ir) {
        Ok(module) => module,
        Err(error) => {
            err!("kira: bytecode compilation failed: {error}");
            return EXIT_FAILURE;
        }
    };

    let bindings = if ir.foreign_imports.is_empty() && ir.foreign_callbacks.is_empty() {
        None
    } else {
        match native::direct_foreign_bindings(ir, source, foreign_link) {
            Ok(bindings) => Some(bindings),
            Err(error) => {
                err!("kira: {error}");
                return EXIT_FAILURE;
            }
        }
    };
    run_vm_module(module, bindings, program_arguments)
}

/// Runs a precompiled bytecode module in the action child.
pub(super) fn run_vm_module_file(
    module_path: &std::path::Path,
    binding_paths: Option<&std::path::Path>,
    program_arguments: &[String],
) -> i32 {
    let bytes = match std::fs::read(module_path) {
        Ok(bytes) => bytes,
        Err(error) => {
            err!(
                "kira test: cannot read `{}`: {error}",
                module_path.display()
            );
            return EXIT_FAILURE;
        }
    };
    let module = match kira_bytecode::Module::from_bytes(&bytes) {
        Ok(module) => module,
        Err(error) => {
            err!(
                "kira test: cannot decode `{}`: {error}",
                module_path.display()
            );
            return EXIT_FAILURE;
        }
    };
    let bindings = match binding_paths {
        Some(path) => match native::read_foreign_binding_paths(path) {
            Ok(paths) => match bindings_from_paths(&module, paths) {
                Ok(bindings) => Some(bindings),
                Err(message) => {
                    err!("kira test: {message}");
                    return EXIT_FAILURE;
                }
            },
            Err(error) => {
                err!("kira test: {error}");
                return EXIT_FAILURE;
            }
        },
        None => None,
    };
    run_vm_module(module, bindings, program_arguments)
}

fn bindings_from_paths(
    module: &kira_bytecode::Module,
    paths: Vec<Option<std::path::PathBuf>>,
) -> Result<Vec<kira_main::ForeignBinding>, String> {
    if paths.len() != module.foreign_imports.len() {
        return Err(format!(
            "foreign binding manifest has {} paths for {} imports",
            paths.len(),
            module.foreign_imports.len()
        ));
    }
    Ok(module
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
        .collect())
}

fn run_vm_module(
    module: kira_bytecode::Module,
    bindings: Option<Vec<kira_main::ForeignBinding>>,
    program_arguments: &[String],
) -> i32 {
    if module.foreign_imports.is_empty() && module.foreign_callbacks.is_empty() {
        kira_diagnostics::progress!("running the program");
        // SAFETY: the CLI owns this run boundary and does not access the
        // process environment from another thread while the VM executes.
        return unsafe {
            env::with_arguments(program_arguments, || {
                let mut host = NativeStateHost::new(StdoutHost);
                match kira_vm_runtime::execute(&module, &mut host) {
                    Ok(outcome) => vm_result_code(outcome.result),
                    Err(trap) => {
                        err!("kira: runtime trap: {trap}");
                        EXIT_FAILURE
                    }
                }
            })
        };
    }

    let Some(imports) = bindings else {
        err!("kira: the bytecode module requires direct foreign-library bindings");
        return EXIT_FAILURE;
    };
    let program = match kira_vm_runtime::Program::load(module) {
        Ok(program) => program,
        Err(error) => {
            err!("kira: {error}");
            return EXIT_FAILURE;
        }
    };
    if imports.len() != program.module().foreign_imports.len() {
        err!("kira: direct foreign binding count does not match the bytecode module");
        return EXIT_FAILURE;
    }
    let callbacks = program
        .module()
        .foreign_callbacks
        .iter()
        .map(|entry| entry.signature().clone())
        .collect();
    let aggregates = program.module().foreign_aggregates.clone();
    let session =
        match kira_main::ForeignSession::load_dynamic(program, imports, callbacks, aggregates) {
            Ok(session) => session,
            Err(error) => {
                err!("kira: cannot load the direct foreign-library session: {error}");
                return EXIT_FAILURE;
            }
        };
    kira_diagnostics::progress!("running the program");
    // SAFETY: this is the same CLI-owned run boundary with the direct foreign
    // libraries loaded.
    match unsafe { env::with_arguments(program_arguments, || session.run()) } {
        Ok(outcome) => vm_result_code(outcome.result),
        Err(trap) => {
            err!("kira: runtime trap: {trap}");
            EXIT_FAILURE
        }
    }
}

fn vm_result_code(result: kira_vm_runtime::Value) -> i32 {
    match result {
        kira_vm_runtime::Value::Int(code) => match i32::try_from(code) {
            Ok(code) => code,
            Err(_) => {
                err!("kira: program returned an Int outside the process exit range");
                EXIT_FAILURE
            }
        },
        _ => EXIT_OK,
    }
}

/// Builds a native executable and runs it, forwarding its exit code.
///
/// Only for a build of this machine. A program emitted for another one is a
/// file this host cannot start, and `run` says so rather than handing the
/// operating system a binary it will refuse with an error naming nothing.
pub(super) fn run_native(
    ir: &IrProgram,
    options: &CompileOptions,
    foreign_link: &NativeLinkInputs,
) -> i32 {
    if let crate::options::Device::Cross(target) = &options.device {
        err!(
            "kira run: `{target}` is not this machine, so the program it builds \
             cannot be run here; use `kira build --target {target}` and run the \
             result on that machine"
        );
        return EXIT_USAGE;
    }
    let Some(artifacts) = build_native(ir, options, foreign_link) else {
        return EXIT_FAILURE;
    };
    let Some(executable) = artifacts.executable else {
        err!("kira run: the native build produced no executable");
        return EXIT_FAILURE;
    };
    kira_diagnostics::progress!("running the program");
    match native::execute(&executable, &options.program_arguments, options.quit_after) {
        Ok(code) => code,
        Err(error) => {
            err!("kira: {error}");
            EXIT_FAILURE
        }
    }
}

/// Builds native artifacts, reporting any backend failure.
pub(super) fn build_native(
    ir: &IrProgram,
    options: &CompileOptions,
    foreign_link: &NativeLinkInputs,
) -> Option<kira_llvm_backend::NativeArtifacts> {
    match native::build(
        ir,
        std::path::Path::new(&options.path),
        options.emit_llvm_ir,
        options.release,
        foreign_link,
        &super::native_build_target(options),
    ) {
        Ok(artifacts) => Some(artifacts),
        Err(error) => {
            err!("kira: {error}");
            None
        }
    }
}
