//! The `run`, `build`, and `check` command pipelines.
//!
//! All three read a single `.kira` file, drive the salsa frontend to collect
//! diagnostics, and render any errors readably against the source. `check`
//! stops there. `run` and `build` continue into the backend `--backend`
//! selects, on the device `--device` selects.
//!
//! The two are independent axes, and `--backend` is never overridden: a device
//! decides only which backend a command that named none gets — the VM on this
//! machine, the module's own code generator on the Web. A pair that is not
//! built yet is refused by name rather than quietly served by another backend,
//! because a user who asked for one engine and measured another has been lied
//! to.
//!
//! The backends:
//!
//! - `vm` compiles the verified IR to bytecode; `run` executes it on the VM
//!   with the [`StdHost`](crate::host::StdHost),
//! - `llvm` compiles the same IR to a native object and links an executable;
//!   `run` then executes that,
//! - `hybrid` splits the same IR on its `@Runtime`/`@Native` annotations and
//!   emits both halves plus a manifest; `run` loads the bundle and runs it in
//!   this process, which is the host.
//!
//! On a `wasm32`/`wasm64` device, `llvm` is the backend that serves it: the
//! wasm backend turns a whole program into machine code for that device exactly
//! as LLVM does for this one, emitting a module plus the page that runs it, and
//! `run` serves them and opens a browser. `vm` and `hybrid` on the Web are not
//! built yet and say so.
//!
//! Every backend consumes the same [`IrProgram`], which is what makes their
//! observable behavior comparable — and what the parity tests check.

use kira_backend_api::BackendMode;
use kira_diagnostics::{Diagnostic, has_errors, renderer};
use kira_ir::IrProgram;
use kira_semantics::{DiagnosticAccumulator, FILE_SOURCE_ID, SourceProgram};
use kira_source::SourceMap;

use kira_wasm_runtime::WasmDevice;

use crate::host::StdHost;
use crate::hybrid;
use crate::native;
use crate::options::{CompileOptions, Device};
use crate::wasm;

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

/// Runs `kirac run [--backend vm|llvm|hybrid] [--device host|wasm32|wasm64]
/// <file>`: report diagnostics, then execute a clean program.
///
/// A wasm device does not run on this machine: it builds a module and serves it
/// to a browser, which is what running a Kira program on the Web means.
pub fn run(args: &[String]) -> i32 {
    let options = match parse_options("run", args) {
        Ok(options) => options,
        Err(code) => return code,
    };
    let ir = match verified_ir("run", &options.path) {
        Ok(ir) => ir,
        Err(code) => return code,
    };

    if let Device::Web(device) = options.device {
        if let Err(code) = web_backend_is_built("run", options.backend, device) {
            return code;
        }
        return run_web(&ir, &options, device);
    }

    match options.backend {
        BackendMode::VmBytecode => run_on_vm(&ir),
        BackendMode::LlvmNative => run_native(&ir, &options),
        BackendMode::Hybrid => run_hybrid(&ir, &options),
    }
}

/// Runs `kirac live [runner] <file> [--backend vm|hybrid]`: build a bundle,
/// serve it, and run it on a runner client.
///
/// Unlike `run`, this does not execute the program in this process: it builds a
/// `.klbundle`, serves it over a socket, and a runner client runs it. That is
/// what makes a live session a live session rather than a run — the app is
/// hosted somewhere that can outlive the compiler and take a new bundle later.
pub fn live(args: &[String]) -> i32 {
    let options = match crate::live::LiveOptions::parse(args) {
        Ok(options) => options,
        Err(error) => {
            eprintln!("kirac live: {error}");
            return EXIT_USAGE;
        }
    };
    let source = std::path::Path::new(&options.path);

    // Compiling is a closure rather than a value, because a watched session
    // rebuilds: the frontend runs again for every save, and a save that does not
    // compile yields `None` rather than an error, so the session keeps the app
    // that is already running.
    let rebuild = || -> Result<Option<kira_live::Bundle>, crate::live::LiveError> {
        match verified_ir("live", &options.path) {
            Ok(ir) => {
                crate::live::build_bundle(&ir, source, options.runner, options.backend).map(Some)
            }
            Err(_) => Ok(None),
        }
    };

    match crate::supervisor::run(&options, source, &rebuild) {
        Ok(()) => EXIT_OK,
        Err(error) => {
            eprintln!("kirac live: {error}");
            EXIT_FAILURE
        }
    }
}

/// Builds a program for the Web and serves it, opening a browser at it.
fn run_web(ir: &IrProgram, options: &CompileOptions, device: WasmDevice) -> i32 {
    match wasm::run(ir, std::path::Path::new(&options.path), device) {
        Ok(()) => EXIT_OK,
        Err(error) => {
            eprintln!("kirac: {error}");
            EXIT_FAILURE
        }
    }
}

/// Whether `backend` is built for a Web device, reporting it if not.
///
/// `--backend` is never overridden: a device decides the default for a command
/// that named no backend, and nothing else. So a backend the Web does not serve
/// yet is refused by name — the alternative is compiling something other than
/// what was asked for and saying nothing, which is how a user comes to believe
/// they measured a VM they never ran.
///
/// `llvm` is the Web's code generator: the wasm backend is what turns a whole
/// program into machine code for that device, exactly as LLVM does for this one.
/// The other two need a VM that runs *in* the module — `kira-vm-runtime` already
/// compiles to `wasm32-unknown-unknown`, so this is wiring rather than a new
/// engine — and, for `hybrid`, a split that reaches a Web device at all.
fn web_backend_is_built(verb: &str, backend: BackendMode, device: WasmDevice) -> Result<(), i32> {
    let missing = match backend {
        BackendMode::LlvmNative => return Ok(()),
        BackendMode::VmBytecode => {
            "the VM on the Web needs the interpreter compiled into the module, \
             which is not wired up yet"
        }
        BackendMode::Hybrid => {
            "a hybrid split does not reach a Web device yet; the whole program \
             compiles to one module"
        }
    };
    eprintln!(
        "kirac {verb}: `--backend {}` is not available for `--device {}` yet: {missing}\n\
         note: `--backend llvm` compiles the program to a WebAssembly module",
        backend.label(),
        device.label(),
    );
    println!("Failed to {verb}");
    Err(EXIT_FAILURE)
}

/// Runs `kirac build [--backend vm|llvm|hybrid] [--device host|wasm32|wasm64]
/// <file>`: compile to artifacts under `.kira-build/`, without executing
/// anything.
pub fn build(args: &[String]) -> i32 {
    let options = match parse_options("build", args) {
        Ok(options) => options,
        Err(code) => return code,
    };
    let ir = match verified_ir("build", &options.path) {
        Ok(ir) => ir,
        Err(code) => return code,
    };

    if let Device::Web(device) = options.device {
        if let Err(code) = web_backend_is_built("build", options.backend, device) {
            return code;
        }
        return match wasm::build(&ir, std::path::Path::new(&options.path), device) {
            Ok(artifacts) => {
                println!("Successfully built {}", artifacts.wasm.display());
                EXIT_OK
            }
            Err(error) => {
                eprintln!("kirac: {error}");
                println!("Failed to build");
                EXIT_FAILURE
            }
        };
    }

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
        BackendMode::Hybrid => match hybrid::build(
            &ir,
            std::path::Path::new(&options.path),
            options.emit_llvm_ir,
        ) {
            Ok(_) => {
                println!("Successfully built");
                EXIT_OK
            }
            Err(error) => {
                eprintln!("kirac: {error}");
                println!("Failed to build");
                EXIT_FAILURE
            }
        },
    }
}

/// Builds a hybrid bundle and runs it in the hybrid host.
///
/// The host runs in this process rather than as a child: the native half is a
/// library, not an executable, and this process is what loads it.
fn run_hybrid(ir: &IrProgram, options: &CompileOptions) -> i32 {
    match hybrid::run(
        ir,
        std::path::Path::new(&options.path),
        options.emit_llvm_ir,
    ) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("kirac: {error}");
            EXIT_FAILURE
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

    // Everything the entry file imports, transitively, dependencies first. An
    // import that names no readable file comes back as nothing here and is
    // reported by the frontend, which has the span to point at.
    let modules = kira_program_graph::load_modules(std::path::Path::new(path), &text);

    // The SourceMap mirrors the salsa input file for file and in the same
    // order, so diagnostic spans render against the file they were written in:
    // the entry file at `FILE_SOURCE_ID`, then module `i` at
    // `module_source_id(i)`.
    let mut sources = SourceMap::new();
    let id = sources
        .insert(path.to_owned(), text.clone())
        .map_err(|full| {
            // One file into an empty map cannot fill it; this is unreachable rather
            // than merely unlikely, but it is reported, not asserted away.
            eprintln!("kirac: {full}");
            EXIT_FAILURE
        })?;
    debug_assert_eq!(id, FILE_SOURCE_ID);
    for (index, module) in modules.iter().enumerate() {
        let id = sources
            .insert(module.path.clone(), module.text.clone())
            .map_err(|full| {
                eprintln!("kirac: {full}");
                EXIT_FAILURE
            })?;
        debug_assert_eq!(id, kira_semantics::module_source_id(index));
    }

    let db = salsa::DatabaseImpl::new();
    let source = SourceProgram::new(&db, text, path.to_owned(), modules);
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
            Vec::new(),
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
            Vec::new(),
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
            Vec::new(),
        );
        // Analyzing directly and through lowering must agree on diagnostics.
        let _ = analyzed(&db, source);
        let diags = lowered::accumulated::<DiagnosticAccumulator>(&db, source);
        assert!(diags.iter().any(|d| d.0.code == Some("KSEM060")));
    }
}
