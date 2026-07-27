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
use kira_runtime_abi::NativeStateHost;

use super::{EXIT_FAILURE, EXIT_OK};
use crate::options::CompileOptions;
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
            eprintln!("kirac: {error}");
            EXIT_FAILURE
        }
    }
}

/// Builds a hybrid bundle and runs it in the hybrid host.
///
/// The host runs in this process rather than as a child: the native half is a
/// library, not an executable, and this process is what loads it.
pub(super) fn run_hybrid(
    ir: &IrProgram,
    options: &CompileOptions,
    foreign_link: &NativeLinkInputs,
) -> i32 {
    match hybrid::run(
        ir,
        std::path::Path::new(&options.path),
        options.emit_llvm_ir,
        foreign_link,
    ) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("kirac: {error}");
            EXIT_FAILURE
        }
    }
}

/// Compiles the IR to bytecode and runs it on the VM.
///
/// A program with foreign imports first builds a foreign-adapter sidecar and
/// runs against a native-capable host that resolves `call_foreign` through it;
/// the VM itself still loads and links nothing. A program with no foreign
/// imports runs against the plain stdout host.
pub(super) fn run_on_vm(
    ir: &IrProgram,
    source: &std::path::Path,
    foreign_link: &NativeLinkInputs,
) -> i32 {
    let module = match kira_bytecode::compile(ir) {
        Ok(module) => module,
        Err(error) => {
            eprintln!("kirac: bytecode compilation failed: {error}");
            return EXIT_FAILURE;
        }
    };

    // A program with no foreign half needs no sidecar; one that hands a Kira
    // function to C needs the same sidecar an import does, because the entry
    // thunk C calls lives in it.
    if ir.foreign_imports.is_empty() && ir.foreign_callbacks.is_empty() {
        let mut host = NativeStateHost::new(StdoutHost);
        return match kira_vm_runtime::execute(&module, &mut host) {
            Ok(_) => EXIT_OK,
            Err(trap) => {
                eprintln!("kirac: runtime trap: {trap}");
                EXIT_FAILURE
            }
        };
    }

    let sidecar = match native::build_adapter_sidecar(ir, source, foreign_link) {
        Ok(path) => path,
        Err(error) => {
            eprintln!("kirac: {error}");
            return EXIT_FAILURE;
        }
    };
    let program = match kira_vm_runtime::Program::load(module) {
        Ok(program) => program,
        Err(error) => {
            eprintln!("kirac: {error}");
            return EXIT_FAILURE;
        }
    };
    let callbacks = (0..ir.foreign_callbacks.len())
        .map(kira_llvm_backend::callback_name)
        .collect();
    let session = match kira_main::ForeignSession::load(
        program,
        &sidecar,
        foreign_bindings(ir),
        callbacks,
        ir.foreign_aggregates.clone(),
    ) {
        Ok(session) => session,
        Err(error) => {
            eprintln!("kirac: cannot load the foreign-adapter sidecar: {error}");
            return EXIT_FAILURE;
        }
    };
    match session.run() {
        Ok(_) => EXIT_OK,
        Err(trap) => {
            eprintln!("kirac: runtime trap: {trap}");
            EXIT_FAILURE
        }
    }
}

/// One adapter binding per foreign import, in import-id order.
///
/// The adapter symbol comes from `kira_llvm_backend::adapter_name`, the one
/// place that contract is spelled, so the sidecar's exports and the host's
/// lookups cannot disagree.
fn foreign_bindings(ir: &IrProgram) -> Vec<kira_main::ForeignBinding> {
    ir.foreign_imports
        .iter()
        .enumerate()
        .map(|(index, entry)| {
            kira_main::ForeignBinding::new(
                kira_llvm_backend::adapter_name(index),
                entry.import.signature().clone(),
            )
        })
        .collect()
}

/// Builds a native executable and runs it, forwarding its exit code.
pub(super) fn run_native(
    ir: &IrProgram,
    options: &CompileOptions,
    foreign_link: &NativeLinkInputs,
) -> i32 {
    let Some(artifacts) = build_native(ir, options, foreign_link) else {
        return EXIT_FAILURE;
    };
    let Some(executable) = artifacts.executable else {
        eprintln!("kirac run: the native build produced no executable");
        return EXIT_FAILURE;
    };
    match native::execute(&executable) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("kirac: {error}");
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
    ) {
        Ok(artifacts) => Some(artifacts),
        Err(error) => {
            eprintln!("kirac: {error}");
            None
        }
    }
}
