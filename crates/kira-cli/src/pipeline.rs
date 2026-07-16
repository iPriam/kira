//! The `run`, `build`, and `check` command pipelines.
//!
//! All three read a single `.kira` file, drive the salsa frontend to collect
//! diagnostics, and render any errors readably against the source. `check`
//! stops there. `run` and `build` continue into a backend, selected by
//! `--backend`:
//!
//! - `vm` compiles the verified IR to bytecode; `run` executes it on the VM
//!   with the [`StdHost`](crate::host::StdHost),
//! - `llvm` compiles the same IR to a native object and links an executable;
//!   `run` then executes that.
//!
//! Both backends consume the same [`IrProgram`], which is what makes their
//! observable behavior comparable — and what the parity tests check.

use kira_backend_api::BackendMode;
use kira_diagnostics::{Diagnostic, has_errors, renderer};
use kira_ir::IrProgram;
use kira_semantics::{DiagnosticAccumulator, FILE_SOURCE_ID, SourceProgram};
use kira_source::SourceMap;

use crate::host::StdHost;
use crate::native;
use crate::options::CompileOptions;

/// Analyzes and lowers the source program to IR, or `None` when it does not
/// type-check to a runnable program (no valid `@Main`).
///
/// This query lives in the CLI (the embedder), not in `kira-ir`: the IR crate
/// sits in the VM's dependency cone, and the portable core must stay
/// salsa-free. It depends on the analyzer query, so all lexer/parser/semantic
/// diagnostics accumulate under it and are gathered with
/// `lowered::accumulated::<DiagnosticAccumulator>`.
#[salsa::tracked(returns(clone))]
fn lowered(db: &dyn salsa::Database, source: SourceProgram) -> Option<IrProgram> {
    let program = kira_semantics::analyzed(db, source);
    kira_ir::lower(&program)
}

/// Process exit code for a clean run.
pub const EXIT_OK: i32 = 0;
/// Process exit code for compile errors, runtime traps, or bad usage.
pub const EXIT_FAILURE: i32 = 1;
/// Process exit code for usage errors (missing arguments, unreadable file).
pub const EXIT_USAGE: i32 = 2;
/// Process exit code for a backend that this build cannot run.
pub const EXIT_UNSUPPORTED: i32 = 2;

/// Runs `kirac check <file>`: report diagnostics, never execute.
pub fn check(args: &[String]) -> i32 {
    let Some(path) = args.first() else {
        eprintln!("kirac check: expected a path to a .kira file");
        return EXIT_USAGE;
    };
    match compile(path) {
        Ok(compiled) => {
            emit_diagnostics(&compiled.diagnostics, &compiled.sources);
            if has_errors(&compiled.diagnostics) {
                EXIT_FAILURE
            } else {
                println!("ok: {path}");
                EXIT_OK
            }
        }
        Err(code) => code,
    }
}

/// Runs `kirac run [--backend vm|llvm] <file>`: report diagnostics, then
/// execute a clean program on the selected backend.
pub fn run(args: &[String]) -> i32 {
    let options = match parse_options("run", args) {
        Ok(options) => options,
        Err(code) => return code,
    };
    let ir = match verified_ir("run", &options.path) {
        Ok(ir) => ir,
        Err(code) => return code,
    };

    match options.backend {
        BackendMode::VmBytecode => run_on_vm(&ir),
        BackendMode::LlvmNative => run_native(&ir, &options),
        BackendMode::Hybrid => {
            eprintln!("kirac run: the hybrid backend is not implemented yet");
            EXIT_UNSUPPORTED
        }
    }
}

/// Runs `kirac build [--backend vm|llvm] <file>`: compile to artifacts under
/// `.kira-build/`, without executing anything.
pub fn build(args: &[String]) -> i32 {
    let options = match parse_options("build", args) {
        Ok(options) => options,
        Err(code) => return code,
    };
    let ir = match verified_ir("build", &options.path) {
        Ok(ir) => ir,
        Err(code) => return code,
    };

    match options.backend {
        BackendMode::VmBytecode => {
            // The VM's artifact is the bytecode module itself; compiling it is
            // the whole build.
            match kira_bytecode::compile(&ir) {
                Ok(_) => {
                    println!("Successfully built");
                    EXIT_OK
                }
                Err(error) => {
                    eprintln!("kirac: bytecode compilation failed: {error}");
                    println!("Failed to build");
                    EXIT_FAILURE
                }
            }
        }
        BackendMode::LlvmNative => match build_native(&ir, &options) {
            Some(_) => {
                println!("Successfully built");
                EXIT_OK
            }
            None => {
                println!("Failed to build");
                EXIT_FAILURE
            }
        },
        BackendMode::Hybrid => {
            eprintln!("kirac build: the hybrid backend is not implemented yet");
            EXIT_UNSUPPORTED
        }
    }
}

/// Compiles the IR to bytecode and runs it on the VM.
fn run_on_vm(ir: &IrProgram) -> i32 {
    let module = match kira_bytecode::compile(ir) {
        Ok(module) => module,
        Err(error) => {
            eprintln!("kirac: bytecode compilation failed: {error}");
            return EXIT_FAILURE;
        }
    };
    let mut host = StdHost;
    match kira_vm_runtime::execute(&module, &mut host) {
        Ok(_) => EXIT_OK,
        Err(trap) => {
            eprintln!("kirac: runtime trap: {trap}");
            EXIT_FAILURE
        }
    }
}

/// Builds a native executable and runs it, forwarding its exit code.
fn run_native(ir: &IrProgram, options: &CompileOptions) -> i32 {
    let Some(artifacts) = build_native(ir, options) else {
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
fn build_native(
    ir: &IrProgram,
    options: &CompileOptions,
) -> Option<kira_llvm_backend::NativeArtifacts> {
    match native::build(
        ir,
        std::path::Path::new(&options.path),
        options.emit_llvm_ir,
    ) {
        Ok(artifacts) => Some(artifacts),
        Err(error) => {
            eprintln!("kirac: {error}");
            None
        }
    }
}

/// Parses shared options, reporting usage errors against `verb`.
fn parse_options(verb: &str, args: &[String]) -> Result<CompileOptions, i32> {
    CompileOptions::parse(args).map_err(|error| {
        eprintln!("kirac {verb}: {error}");
        EXIT_USAGE
    })
}

/// Compiles `path` and returns its IR, or the exit code to report.
///
/// Diagnostics are rendered here, so callers only decide what to do with a
/// program that is known good.
fn verified_ir(verb: &str, path: &str) -> Result<IrProgram, i32> {
    let compiled = compile(path)?;
    emit_diagnostics(&compiled.diagnostics, &compiled.sources);
    if has_errors(&compiled.diagnostics) {
        return Err(EXIT_FAILURE);
    }
    compiled.ir.ok_or_else(|| {
        // No IR without errors should be impossible, but never build or
        // execute nothing.
        eprintln!("kirac {verb}: program has nothing to execute");
        EXIT_FAILURE
    })
}

/// A compiled program plus everything needed to report on it.
struct Compiled {
    sources: SourceMap,
    diagnostics: Vec<Diagnostic>,
    ir: Option<IrProgram>,
}

/// Reads and compiles `path` through the salsa frontend and IR lowering.
///
/// Returns `Err(exit_code)` only for I/O problems (a missing or unreadable
/// file); compile errors are carried as diagnostics, not as an error here.
fn compile(path: &str) -> Result<Compiled, i32> {
    let text = std::fs::read_to_string(path).map_err(|error| {
        eprintln!("kirac: cannot read `{path}`: {error}");
        EXIT_USAGE
    })?;

    // The SourceMap mirrors the salsa input under the same fixed id, so
    // diagnostic spans render against the file text.
    let mut sources = SourceMap::new();
    let id = sources.insert(path.to_owned(), text.clone());
    debug_assert_eq!(id, FILE_SOURCE_ID);

    let db = salsa::DatabaseImpl::new();
    let source = SourceProgram::new(&db, text, path.to_owned());
    let ir = lowered(&db, source);
    let diagnostics = lowered::accumulated::<DiagnosticAccumulator>(&db, source)
        .into_iter()
        .map(|accumulated| accumulated.0.clone())
        .collect();

    Ok(Compiled {
        sources,
        diagnostics,
        ir,
    })
}

/// Renders every diagnostic to stderr in source order.
fn emit_diagnostics(diagnostics: &[Diagnostic], sources: &SourceMap) {
    for diagnostic in diagnostics {
        eprint!("{}", renderer::render(diagnostic, sources));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kira_semantics::analyzed;

    #[test]
    fn lowers_a_clean_program() {
        let db = salsa::DatabaseImpl::new();
        let source = SourceProgram::new(
            &db,
            "@Main function main() { print(1) return }".to_owned(),
            "test.kira".to_owned(),
        );
        let ir = lowered(&db, source).expect("a runnable program");
        assert_eq!(ir.functions.len(), 1);
        assert_eq!(ir.main, 0);
    }

    #[test]
    fn a_program_without_main_does_not_lower() {
        let db = salsa::DatabaseImpl::new();
        let source = SourceProgram::new(
            &db,
            "function f() { return }".to_owned(),
            "t.kira".to_owned(),
        );
        assert!(lowered(&db, source).is_none());
        // The missing-main diagnostic still surfaces through the accumulator.
        let diags = lowered::accumulated::<DiagnosticAccumulator>(&db, source);
        assert!(diags.iter().any(|d| d.0.code == Some("KSEM011")));
    }

    #[test]
    fn diagnostics_propagate_through_lowering() {
        let db = salsa::DatabaseImpl::new();
        let source = SourceProgram::new(
            &db,
            "@Main function main() { print(missing) return }".to_owned(),
            "t.kira".to_owned(),
        );
        // Analyzing directly and through lowering must agree on diagnostics.
        let _ = analyzed(&db, source);
        let diags = lowered::accumulated::<DiagnosticAccumulator>(&db, source);
        assert!(diags.iter().any(|d| d.0.code == Some("KSEM060")));
    }
}
