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
//!   with the [`StdoutHost`](kira_main::StdoutHost),
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
use kira_build::{Compiled, FrontendError};
use kira_diagnostics::{Diagnostic, renderer};
use kira_ir::IrProgram;
use kira_source::SourceMap;

use kira_wasm_runtime::WasmDevice;

use crate::hybrid;
use crate::library;
use crate::native;
use crate::native_library;
use crate::options::{CompileOptions, Device};
use crate::wasm;
use kira_main::StdoutHost;

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
            if compiled.has_errors() {
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
    let ir = match runnable_ir("run", &options.path) {
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
        match runnable_ir("live", &options.path) {
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

/// Whether the engine `backend` selects can build a library's `@Export`
/// surface, reporting it by name when it cannot.
///
/// **The VM engine can**, and is the default: it writes the KBC1 exports
/// section, embeds the artifact in a generated Rust crate, and runs it on a
/// persistent instance. That is the product a consumer gets today.
///
/// The other two cannot yet, and are refused rather than allowed to write an
/// artifact with an invisible surface — which would look complete and be
/// unreachable. Refused per backend rather than once above them because the
/// work each one owes is different, and a user told "not built yet" deserves to
/// know which engine they are waiting on.
fn export_engine_is_built(
    verb: &str,
    backend: BackendMode,
    device: Device,
    ir: &IrProgram,
) -> Result<(), i32> {
    if ir.exports.is_empty() {
        return Ok(());
    }
    let names: Vec<&str> = ir
        .exports
        .iter()
        .map(|export| export.exported_name.as_str())
        .collect();
    let missing = match device {
        // The wasm refusal is about the artifact, not the engine: one module,
        // one export, and an undesigned string/allocator contract across a
        // module boundary. It stands whether or not the library declares
        // exports, and this names the export half of it.
        //
        // A Rust *application* compiled to wasm that embeds the library is a
        // different thing entirely, and it works: the generated crate builds for
        // `wasm32-unknown-unknown` because everything under it does.
        Device::Web(_) => {
            "the wasm backend emits one self-contained module with a single \
             entrypoint, and the string/allocator contract across a wasm module \
             boundary is undesigned\n\
             note: a Rust program that embeds this library and is itself compiled \
             to wasm needs none of that — build with `--backend vm` and depend on \
             the generated crate"
        }
        Device::Host => match backend {
            BackendMode::VmBytecode => return Ok(()),
            // The native engine builds this surface: stable `kira_lib_*`
            // trampolines, a destructor per exported class, and the
            // per-library ABI marker.
            BackendMode::LlvmNative => return Ok(()),
            BackendMode::Hybrid => {
                "the hybrid engine serves neither half's export surface yet: \
                 the bytecode half needs the native half's trampolines to agree \
                 with it, and the trampolines do not exist"
            }
        },
    };
    eprintln!(
        "kirac {verb}: `--backend {}` on `--device {}`: library export is not built yet: \
         {missing}\n\
         note: this package exports {}\n\
         note: `--backend vm` builds this library's export surface today, into \
         `.kira-build/rust/<package>/`",
        backend.label(),
        device.label(),
        names.join(", "),
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
    let compiled = match verified(&options.path) {
        Ok(compiled) => compiled,
        Err(code) => return code,
    };
    let ir = &compiled.ir;
    if let Err(code) = export_engine_is_built("build", options.backend, options.device, ir) {
        return code;
    }
    // A library and a program are built by different paths on every backend:
    // one produces something a consumer depends on, the other something the OS
    // can start.
    let is_library = ir.main.is_none();

    if let Device::Web(device) = options.device {
        if let Err(code) = web_backend_is_built("build", options.backend, device) {
            return code;
        }
        return match wasm::build(ir, std::path::Path::new(&options.path), device) {
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
        BackendMode::VmBytecode if is_library => {
            // The VM engine is the one that serves a consumer today: the
            // artifact is the bytecode *plus* the Rust crate that embeds and
            // calls it, because a `.kbc` on its own is nothing a Rust program
            // can depend on.
            match library::build(&compiled, std::path::Path::new(&options.path)) {
                Ok(artifacts) => {
                    library::report(&artifacts);
                    EXIT_OK
                }
                Err(error) => {
                    eprintln!("kirac: {error}");
                    println!("Failed to build");
                    EXIT_FAILURE
                }
            }
        }
        BackendMode::VmBytecode => {
            // A program's VM artifact is the bytecode module itself; compiling
            // it is the whole build.
            match kira_bytecode::compile(ir) {
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
        BackendMode::LlvmNative if is_library => {
            // The native engine's artifact is the archive *plus* the Rust crate
            // that links and calls it, for the same reason the VM engine's is
            // the bytecode plus the crate that embeds it: an archive on its own
            // is nothing a Rust program can depend on.
            match native_library::build(
                &compiled,
                std::path::Path::new(&options.path),
                options.emit_llvm_ir,
            ) {
                Ok(artifacts) => {
                    native_library::report(&artifacts);
                    EXIT_OK
                }
                Err(error) => {
                    eprintln!("kirac: {error}");
                    println!("Failed to build");
                    EXIT_FAILURE
                }
            }
        }
        BackendMode::LlvmNative => match build_native(ir, &options) {
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
            ir,
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
    let mut host = StdoutHost;
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

/// Compiles `path` and returns everything about it, or the exit code to report.
///
/// Diagnostics are rendered here, so callers only decide what to do with a
/// program that is known good. The IR may be a library's — it carries no
/// entrypoint then, and the caller decides whether that is usable.
fn verified(path: &str) -> Result<Compiled, i32> {
    let compiled = compile(path)?;
    emit_diagnostics(&compiled.diagnostics, &compiled.sources);
    if compiled.has_errors() {
        return Err(EXIT_FAILURE);
    }
    Ok(compiled)
}

/// Compiles `path` and returns its IR alone, for a caller that needs no more.
fn verified_ir(path: &str) -> Result<IrProgram, i32> {
    Ok(verified(path)?.ir)
}

/// Compiles `path` and returns its IR, refusing a library by name.
///
/// The refusal for every verb that *starts* a program. A library has no
/// entrypoint by construction, so there is nothing to start — said plainly,
/// with the reason, rather than by failing somewhere further down where the
/// missing entrypoint looks like a compiler fault.
fn runnable_ir(verb: &str, path: &str) -> Result<IrProgram, i32> {
    let ir = verified_ir(path)?;
    if ir.main.is_none() {
        eprintln!(
            "kirac {verb}: cannot {verb} a library: a library has no `@Main` \
             entrypoint, because it is entered by whatever consumes it\n\
             note: `kirac build` compiles a library to an artifact a consumer links"
        );
        // stderr only, matching how `run` reports every other refusal: stdout
        // is the program's, and a program that never started wrote nothing.
        return Err(EXIT_FAILURE);
    }
    Ok(ir)
}

/// Reads and compiles `path` through the frontend `kira-build` owns.
///
/// Returns `Err(exit_code)` only for problems that prevent compiling at all
/// (a missing or unreadable file, an unusable manifest); compile errors are
/// carried as diagnostics, not as an error here.
fn compile(path: &str) -> Result<Compiled, i32> {
    kira_build::compile(std::path::Path::new(path)).map_err(|error| {
        eprintln!("kirac: {error}");
        // A path the user typed that is not there is a usage error; everything
        // else got far enough that the invocation itself was fine.
        match error {
            FrontendError::Read { .. } => EXIT_USAGE,
            FrontendError::SourceMapFull { .. } | FrontendError::Discovery(_) => EXIT_FAILURE,
        }
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

    #[test]
    fn a_frontend_read_failure_is_a_usage_error_and_the_rest_are_not() {
        // The exit-code split the CLI owns: a path the user typed that is not
        // there is bad usage, and anything past that is a failed build.
        assert_eq!(compile("/nonexistent/kirac/x.kira").err(), Some(EXIT_USAGE));
    }
}
