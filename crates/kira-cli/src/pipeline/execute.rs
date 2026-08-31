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
use kira_runtime_abi::{LinuxSyscall, NativeStateHost, env};

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

/// The system calls `ir` names that no interpreter can serve, each once.
///
/// The interpreter emits no instruction: it asks its host, through the same
/// `HostCapabilities` seam it reaches a file or a native function by, and the
/// host makes the call in the process it is standing in. That is enough for a
/// call that acts on a file descriptor and says what it did — a `--backend vm`
/// run of such a program can be compared byte for byte against a native one,
/// which is the oracle these programs otherwise have none of.
///
/// It is not enough for the rest, and
/// [`LinuxSyscall::servable_by_an_interpreter`] carries the reason each is
/// refused: under the interpreter the process is the interpreter, not the
/// program, so `execve` replaces the VM's own image, `exit_group` ends it
/// mid-program, `wait4` reaps its children, and `mount`, `reboot` and `sync` act
/// on the machine the developer is sitting at.
///
/// `sync` is the one that has to be argued rather than seen. It is filesystem
/// work, so it reads as descriptor work — but it takes no descriptor and writes
/// back every mount on the box, which is the `mount`/`reboot` case exactly.
///
/// Decided from what the program *names* rather than from what it reaches: an
/// import table has no call graph in it, and a package like `packages/linux`
/// that declares the whole kernel surface is refused whether or not this
/// program calls the seven.
pub(crate) fn unservable_syscalls(ir: &IrProgram) -> Vec<LinuxSyscall> {
    let mut refused: Vec<LinuxSyscall> = Vec::new();
    for call in ir
        .foreign_imports
        .iter()
        .filter_map(|entry| entry.import.as_syscall())
    {
        // Once per call rather than once per declaration: two wrappers around
        // the same kernel entry are one thing to say about this program.
        if !call.servable_by_an_interpreter() && !refused.contains(&call) {
            refused.push(call);
        }
    }
    refused
}

/// What to tell an author who pointed the interpreter at `refused`.
///
/// One sentence, built here rather than at each command that reports it, so
/// `run`, `test` and `debug` cannot come to say different things about the same
/// program. Each call is named with what it would have done, because "the VM
/// cannot do this" leaves the reader to guess whether the fix is the program or
/// the command line — and both engines that do work are named for the same
/// reason.
pub(crate) fn syscall_refusal(refused: &[LinuxSyscall]) -> String {
    let named = refused
        .iter()
        .map(|call| format!("`{}` {}", call.label(), call.interpreter_refusal()))
        .collect::<Vec<_>>()
        .join("; ");
    format!(
        "this program calls the Linux kernel directly, and the VM engine cannot serve every call \
         it names: under the interpreter the process is the interpreter rather than the program, \
         so {named}. Run it with `--backend llvm` or `--backend hybrid`, both of which emit the \
         kernel entry sequence into the program's own image."
    )
}

/// Refuses the pure interpreter such a program and answers whether it did.
///
/// Refused before the program starts, for the same reason `run` refuses a cross
/// target by name: the engine is chosen on the command line, so the fix is on
/// the command line, and a program that has already begun writing output is past
/// the point where that can be said.
pub(super) fn refuse_syscalls_on_the_vm(ir: &IrProgram) -> bool {
    let refused = unservable_syscalls(ir);
    if refused.is_empty() {
        return false;
    }
    err!("kira: {}", syscall_refusal(&refused));
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
        options.sanitize,
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
            // The module is what says a row is a system call, not the manifest:
            // the ABI travelled into the `.kbc` with the import table, so it is
            // already here and a second token in the manifest would be a second
            // place for it to be wrong.
            if let Some(call) = entry.as_syscall() {
                return kira_main::ForeignBinding::syscall(call, entry.signature().clone());
            }
            // An address import binds its symbol exactly as a call does; what
            // differs is that nothing is invoked after the lookup. The manifest
            // carries that in the ABI, and it has to survive being rebuilt here
            // or the session calls an object's first bytes.
            let binding = path.map_or_else(
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
            );
            if entry.abi().answers_an_address() {
                binding.answering_address()
            } else {
                binding
            }
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
                match kira_vm_runtime::execute_with_main_thread(&module, &mut host) {
                    Ok(outcome) => vm_outcome_code(outcome),
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
        Ok(outcome) => vm_outcome_code(outcome),
        Err(trap) => {
            err!("kira: runtime trap: {trap}");
            EXIT_FAILURE
        }
    }
}

/// Turns a completed run into a process exit code, refusing a leaked heap.
///
/// The VM's contract is that a run reclaims every allocation: `current` counts
/// live objects at exit and `retained` the values a `retains:` foreign
/// parameter deliberately handed to C, so anything past `retained` is storage
/// the run allocated and lost. Kira's promise is that a compiled program does
/// not leak; a run that ends unbalanced is a compiler or runtime defect, and
/// failing here is what keeps every VM run in the test corpus a proof of that
/// promise rather than a hope.
fn vm_outcome_code(outcome: kira_vm_runtime::RunOutcome) -> i32 {
    if outcome.heap.current != outcome.heap.retained {
        err!(
            "kira: the run leaked {} heap object(s) ({} allocated, {} freed, {} retained); \
             this is a toolchain defect — please report it",
            outcome.heap.current - outcome.heap.retained,
            outcome.heap.allocated,
            outcome.heap.freed,
            outcome.heap.retained,
        );
        return EXIT_FAILURE;
    }
    match outcome.result {
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
        options.sanitize,
    ) {
        Ok(artifacts) => Some(artifacts),
        Err(error) => {
            err!("kira: {error}");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The refusal names each call with what it would have done, and offers the
    /// two engines that do work.
    ///
    /// A message that said only "the VM cannot make system calls" would now be
    /// wrong as well as unhelpful — three of them it can — so what a reader has
    /// to be given is the effect that makes *this* call the interpreter's
    /// business rather than the program's.
    #[test]
    fn the_refusal_names_every_call_with_the_effect_that_refuses_it() {
        let message = syscall_refusal(&[LinuxSyscall::Mount, LinuxSyscall::ExitGroup]);
        assert!(
            message.contains("`mount` would mount a filesystem"),
            "{message}"
        );
        assert!(
            message.contains("`exit_group` would end the interpreter itself"),
            "{message}"
        );
        assert!(
            message.contains("the process is the interpreter rather than the program"),
            "{message}"
        );
        assert!(
            message.contains("--backend llvm") && message.contains("--backend hybrid"),
            "{message}"
        );
    }

    /// A served call never appears in a refusal, whatever else does. Naming one
    /// would send an author to change a call that works on this engine.
    #[test]
    fn a_refusal_never_names_a_call_the_interpreter_serves() {
        let message = syscall_refusal(&[LinuxSyscall::Execve, LinuxSyscall::Wait4]);
        for served in ["`read`", "`write`", "`ppoll`"] {
            assert!(!message.contains(served), "{served} in: {message}");
        }
    }
}
