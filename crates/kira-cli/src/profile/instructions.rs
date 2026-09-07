//! The `instructions` event: exactly what the interpreter executed.
//!
//! Unlike the sampled events, this one runs the program in this process with an
//! observer on every instruction. It is therefore the only event that never
//! launches a child, and the only one whose numbers are counts rather than
//! estimates.
//!
//! The same module also disassembles a program for `annotate`, because both
//! answer the same question — what is at instruction *n* of function *f* — and
//! both need the bytecode a recording only names by its source.

use std::collections::HashMap;
use std::path::Path;

use kira_backend_api::BackendMode;
use kira_ir::IrProgram;
use kira_llvm_backend::NativeLinkInputs;
use kira_main::StdoutHost;
use kira_profile::counters::{InstructionCounter, InstructionProfile};
use kira_profile::render::annotate::SiteText;
use kira_runtime_abi::{NativeStateHost, env};

use crate::pipeline::EXIT_FAILURE;
use crate::progress::err;

/// Counts every instruction a run of `ir` executes.
///
/// The foreign half is loaded exactly as an ordinary run loads it, so a program
/// with `@FFI.Extern` imports counts the same instructions it would otherwise
/// execute rather than trapping at the first crossing.
pub(super) fn count(
    ir: &IrProgram,
    source: &Path,
    backend: BackendMode,
    foreign_link: &NativeLinkInputs,
    program_arguments: &[String],
    emit_llvm_ir: bool,
) -> Result<InstructionProfile, i32> {
    let mut counter = InstructionCounter::new();
    let outcome = match backend {
        BackendMode::Hybrid => count_hybrid(
            ir,
            source,
            foreign_link,
            program_arguments,
            emit_llvm_ir,
            &mut counter,
        ),
        _ => count_vm(ir, source, foreign_link, program_arguments, &mut counter),
    }?;
    if let Err(trap) = outcome {
        err!("kira profile record: runtime trap: {trap}");
        return Err(EXIT_FAILURE);
    }
    Ok(counter.finish())
}

/// The result of a counted run: an inner error is the program's trap, an outer
/// one is the profiler failing to start it.
type Counted = Result<Result<(), String>, i32>;

fn count_vm(
    ir: &IrProgram,
    source: &Path,
    foreign_link: &NativeLinkInputs,
    program_arguments: &[String],
    counter: &mut InstructionCounter,
) -> Counted {
    let module = kira_bytecode::compile(ir).map_err(|error| {
        err!("kira profile record: bytecode compilation failed: {error}");
        EXIT_FAILURE
    })?;
    if ir.foreign_imports.is_empty() && ir.foreign_callbacks.is_empty() {
        let mut host = NativeStateHost::new(StdoutHost);
        // SAFETY: the profiler owns this run boundary and does not access the
        // process environment from another thread while the VM executes.
        let outcome = unsafe {
            env::with_arguments(program_arguments, || {
                kira_vm_runtime::execute_with_main_thread_debug(&module, &mut host, counter)
                    .map(|_| ())
            })
        };
        return Ok(outcome.map_err(|trap| trap.to_string()));
    }

    let imports =
        crate::native::direct_foreign_bindings(ir, source, foreign_link).map_err(|error| {
            err!("kira profile record: {error}");
            EXIT_FAILURE
        })?;
    let program = kira_vm_runtime::Program::load(module).map_err(|error| {
        err!("kira profile record: {error}");
        EXIT_FAILURE
    })?;
    let session = kira_main::ForeignSession::load_dynamic(
        program,
        imports,
        ir.foreign_callbacks
            .iter()
            .map(|callback| callback.signature().clone())
            .collect(),
        ir.foreign_aggregates.clone(),
    )
    .map_err(|error| {
        err!("kira profile record: cannot load the direct foreign-library session: {error}");
        EXIT_FAILURE
    })?;
    // SAFETY: the same profiler-owned run boundary, with direct libraries loaded.
    let outcome = unsafe {
        env::with_arguments(program_arguments, || {
            session.run_with_debug(counter).map(|_| ())
        })
    };
    Ok(outcome.map_err(|trap| trap.to_string()))
}

fn count_hybrid(
    ir: &IrProgram,
    source: &Path,
    foreign_link: &NativeLinkInputs,
    program_arguments: &[String],
    emit_llvm_ir: bool,
    counter: &mut InstructionCounter,
) -> Counted {
    let bundle = crate::hybrid::build(
        ir,
        source,
        emit_llvm_ir,
        kira_llvm_backend::Sanitize::None,
        foreign_link,
    )
    .map_err(|error| {
        err!("kira profile record: {error}");
        EXIT_FAILURE
    })?;
    let session = kira_hybrid_runtime::Session::load(&bundle.manifest).map_err(|error| {
        err!("kira profile record: {error}");
        EXIT_FAILURE
    })?;
    // SAFETY: the profiler owns this run boundary and does not access the
    // process environment from another thread while the bundle executes.
    let outcome =
        unsafe { env::with_arguments(program_arguments, || session.run_with_debug(counter)) };
    Ok(outcome.map_err(|error| error.to_string()))
}

/// The instruction at each site of a compiled program.
#[derive(Debug, Default)]
pub(super) struct Disassembly {
    sites: HashMap<(u32, u32), String>,
}

impl SiteText for Disassembly {
    fn text(&self, function: u32, offset: u32) -> Option<String> {
        self.sites.get(&(function, offset)).cloned()
    }
}

/// Compiles `source` and reads back what is at every instruction site.
///
/// Best effort by construction: `annotate` is useful without it, and a
/// recording whose source has since changed or moved should still annotate the
/// offsets it recorded rather than refuse.
pub(super) fn disassemble(source: &Path) -> Option<Disassembly> {
    let target = crate::foreign_libs::target_for_device(&crate::options::Device::Host);
    let compiled = kira_build::compile_for(source, None, &target).ok()?;
    if compiled.has_errors() {
        return None;
    }
    let module = kira_bytecode::compile(&compiled.ir).ok()?;
    let mut sites = HashMap::new();
    for (index, function) in module.functions.iter().enumerate() {
        for (pc, instruction) in function.code.iter().enumerate() {
            sites.insert((index as u32, pc as u32), format!("{instruction:?}"));
        }
    }
    Some(Disassembly { sites })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_source_that_does_not_compile_annotates_without_instruction_text() {
        assert!(disassemble(Path::new("/nonexistent/kira/x.kira")).is_none());
    }
}
