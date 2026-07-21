//! The `run`, `build`, and `check` command pipelines.
//!
//! All three resolve a `.kira` file or package directory, drive the salsa
//! frontend to collect diagnostics, and render any errors readably against the source. `check`
//! stops there. `run` and `build` continue into the backend `--backend`
//! selects, on the device `--device` selects.
//!
//! `--device` is an override. On the host, `--backend` picks the engine; a
//! Web device has exactly one code generator, so naming the device decides
//! the backend, and a differing `--backend` beside it is overridden aloud —
//! never served, never silently swapped.
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
//! On a `wasm32`/`wasm64` device, the wasm backend turns the whole program
//! into machine code for that device exactly as LLVM does for this one,
//! emitting a module plus the page that runs it; `run` serves them and opens a
//! browser.
//!
//! Every backend consumes the same [`IrProgram`], which is what makes their
//! observable behavior comparable — and what the parity tests check.

use kira_backend_api::BackendMode;
use kira_build::{Compiled, FrontendError};
use kira_diagnostics::{Diagnostic, renderer};
use kira_ir::IrProgram;
use kira_source::SourceMap;

use kira_backend_api::WasmDevice;

use crate::hybrid;
use crate::hybrid_library;
use crate::library;
use crate::native;
use crate::native_library;
use crate::options::{CompileOptions, Device};
use crate::wasm;

mod execute;

use self::execute::{build_native, run_hybrid, run_native, run_on_vm, run_web};

/// Process exit code for a clean run.
pub const EXIT_OK: i32 = 0;
/// Process exit code for compile errors, runtime traps, or bad usage.
pub const EXIT_FAILURE: i32 = 1;
/// Process exit code for usage errors (missing arguments, unreadable file).
pub const EXIT_USAGE: i32 = 2;

/// Runs `kirac check <file|dir>`: report diagnostics, never execute.
pub fn check(args: &[String]) -> i32 {
    let Some(path) = args.first() else {
        eprintln!("kirac check: expected a path to a .kira file or package directory");
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
/// <file|dir>`: report diagnostics, then execute a clean program.
///
/// A wasm device does not run on this machine: it builds a module and serves it
/// to a browser, which is what running a Kira program on the Web means.
pub fn run(args: &[String]) -> i32 {
    let mut options = match parse_options("run", args) {
        Ok(options) => options,
        Err(code) => return code,
    };
    options.path = match resolve_path(&options.path) {
        Ok(path) => path,
        Err(code) => return code,
    };
    let compiled = match verified(&options.path) {
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

    let foreign = match resolve_foreign(&options.path, &ir, options.device) {
        Ok(foreign) => foreign,
        Err(code) => return code,
    };
    let archives = foreign_archives(&foreign);

    if let Device::Web(device) = options.device {
        return run_web(&ir, &options, device, archives);
    }

    match options.backend {
        BackendMode::VmBytecode => run_on_vm(&ir, std::path::Path::new(&options.path), archives),
        BackendMode::LlvmNative => run_native(&ir, &options, archives),
        BackendMode::Hybrid => run_hybrid(&ir, &options, archives),
    }
}

/// Resolves the program's `@FFI.Extern` imports to archives for `device`'s
/// target, reporting a resolution failure as an exit code.
///
/// `None` when the program declares no foreign imports.
fn resolve_foreign(
    source: &str,
    ir: &IrProgram,
    device: Device,
) -> Result<Option<Vec<std::path::PathBuf>>, i32> {
    let target = crate::foreign_libs::target_for_device(device);
    crate::foreign_libs::resolve(std::path::Path::new(source), ir, target).map_err(|error| {
        eprintln!("kirac: {error}");
        EXIT_FAILURE
    })
}

/// The archive slice a backend links, empty when there are no foreign imports.
fn foreign_archives(foreign: &Option<Vec<std::path::PathBuf>>) -> &[std::path::PathBuf] {
    foreign.as_ref().map(Vec::as_slice).unwrap_or(&[])
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
        match runnable_path_ir("live", &options.path) {
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

/// Whether the engine `backend` selects can build a library's `@Export`
/// surface, reporting it by name when it cannot.
///
/// **All three host engines can**, and each produces the same generated Rust
/// API over a different engine underneath — which is where this feature's parity
/// is measured. The VM engine is the default and embeds a `.kbc`; the native
/// engine emits `kira_lib_*` trampolines into an archive; the hybrid engine
/// embeds a `.kbc` plus the `.khm` describing the `@Runtime`/`@Native` split and
/// loads a shared library beside it.
///
/// What remains refused is the **wasm library artifact**, and it is refused for
/// the artifact rather than for any engine: see the arm below. A Rust program
/// that embeds a Kira library and is *itself* compiled to wasm is a different
/// thing and works.
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
        // Every host engine builds this surface. Matched exhaustively rather
        // than waved through with a wildcard, so a fourth backend has to decide
        // what it does here instead of inheriting a yes.
        Device::Host => match backend {
            // Embeds the `.kbc` and runs it on a persistent instance.
            BackendMode::VmBytecode => return Ok(()),
            // Stable `kira_lib_*` trampolines, a destructor per exported class,
            // and the per-library ABI marker, in an archive the consumer links.
            BackendMode::LlvmNative => return Ok(()),
            // The consumer enters the bytecode half, which calls into the native
            // half through the seam an application already uses — so this is the
            // one engine where a library's own `@Runtime`/`@Native` annotations
            // still mean something.
            BackendMode::Hybrid => return Ok(()),
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
/// <file|dir>`: compile to artifacts under `.kira-build/`, without executing
/// anything.
pub fn build(args: &[String]) -> i32 {
    let mut options = match parse_options("build", args) {
        Ok(options) => options,
        Err(code) => return code,
    };
    options.path = match resolve_path(&options.path) {
        Ok(path) => path,
        Err(code) => return code,
    };
    let compiled = match verified(&options.path) {
        Ok(compiled) => compiled,
        Err(code) => return code,
    };
    if let Err(code) = apply_manifest_defaults("build", &mut options, &compiled) {
        return code;
    }
    let ir = &compiled.ir;
    if let Err(code) = export_engine_is_built("build", options.backend, options.device, ir) {
        return code;
    }
    // A library and a program are built by different paths on every backend:
    // one produces something a consumer depends on, the other something the OS
    // can start.
    let is_library = ir.main.is_none();

    // Resolve the program's foreign imports to archives for the selected target
    // once, and thread them into whichever backend runs. A library's foreign
    // surface is not built in this milestone, so only program arms link them.
    let foreign = match resolve_foreign(&options.path, ir, options.device) {
        Ok(foreign) => foreign,
        Err(_) => {
            println!("Failed to build");
            return EXIT_FAILURE;
        }
    };
    let archives = foreign_archives(&foreign);

    if let Device::Web(device) = options.device {
        return match wasm::build(ir, std::path::Path::new(&options.path), device, archives) {
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
            // it is the whole build. A program with foreign imports also emits
            // the adapter sidecar a VM run loads, so `build` and `run` produce
            // the same artifacts.
            match kira_bytecode::compile(ir) {
                Ok(_) => {
                    if !ir.foreign_imports.is_empty()
                        && let Err(error) = native::build_adapter_sidecar(
                            ir,
                            std::path::Path::new(&options.path),
                            archives,
                        )
                    {
                        eprintln!("kirac: {error}");
                        println!("Failed to build");
                        return EXIT_FAILURE;
                    }
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
        BackendMode::LlvmNative => match build_native(ir, &options, archives) {
            Some(_) => {
                println!("Successfully built");
                EXIT_OK
            }
            None => {
                println!("Failed to build");
                EXIT_FAILURE
            }
        },
        BackendMode::Hybrid if is_library => {
            // Three artifacts plus the crate, and the only engine that keeps the
            // author's `@Runtime`/`@Native` split meaningful in a library: the
            // consumer enters the bytecode half, which calls into the native
            // half through the seam an application already uses.
            match hybrid_library::build(
                &compiled,
                std::path::Path::new(&options.path),
                options.emit_llvm_ir,
            ) {
                Ok(artifacts) => {
                    hybrid_library::report(&artifacts);
                    EXIT_OK
                }
                Err(error) => {
                    eprintln!("kirac: {error}");
                    println!("Failed to build");
                    EXIT_FAILURE
                }
            }
        }
        BackendMode::Hybrid => match hybrid::build(
            ir,
            std::path::Path::new(&options.path),
            options.emit_llvm_ir,
            archives,
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

/// Parses shared options, reporting usage errors against `verb`.
fn parse_options(verb: &str, args: &[String]) -> Result<CompileOptions, i32> {
    CompileOptions::parse(args).map_err(|error| {
        eprintln!("kirac {verb}: {error}");
        EXIT_USAGE
    })
}

/// Resolves a package directory to the source file that seeds compilation.
fn resolve_path(path: &str) -> Result<String, i32> {
    let target = kira_project::resolve_target(std::path::Path::new(path)).map_err(|error| {
        eprintln!("kirac: {error}");
        match error {
            kira_project::DiscoveryError::NotPackageDirectory { .. }
            | kira_project::DiscoveryError::MissingEntrypoint { .. }
            | kira_project::DiscoveryError::NoLibrarySources { .. } => EXIT_USAGE,
            kira_project::DiscoveryError::Unreadable { .. }
            | kira_project::DiscoveryError::Malformed { .. } => EXIT_FAILURE,
        }
    })?;
    target.source_path.ok_or_else(|| {
        eprintln!("kirac: `{path}` did not resolve to a compilable Kira source");
        EXIT_USAGE
    })
}

/// Applies package defaults without replacing any command-line choice.
fn apply_manifest_defaults(
    verb: &str,
    options: &mut CompileOptions,
    compiled: &Compiled,
) -> Result<(), i32> {
    // A manifest target can choose the backend indirectly (Web has only LLVM),
    // so any explicit backend must outrank it just as an explicit device does.
    if !options.device_explicit
        && !options.backend_explicit
        && let Some(target) = compiled.default_build_target.as_deref()
    {
        options.device = manifest_device(target).ok_or_else(|| {
            eprintln!("kirac {verb}: unknown manifest build target `{target}`");
            EXIT_FAILURE
        })?;
    }

    if matches!(options.device, Device::Host) {
        if !options.backend_explicit
            && let Some(mode) = compiled.default_execution_mode.as_deref()
        {
            options.backend = manifest_backend(mode).ok_or_else(|| {
                eprintln!("kirac {verb}: unknown manifest execution mode `{mode}`");
                EXIT_FAILURE
            })?;
        }
    } else {
        if options.backend_explicit && options.backend != BackendMode::LlvmNative {
            eprintln!(
                "kirac: `--device {}` overrides `--backend {}`: the Web device has one code generator",
                options.device.label(),
                options.backend.label(),
            );
        }
        options.backend = BackendMode::LlvmNative;
    }
    Ok(())
}

/// Maps a manifest execution mode to the backend API.
fn manifest_backend(mode: &str) -> Option<BackendMode> {
    match mode {
        "vm" => Some(BackendMode::VmBytecode),
        "llvm" => Some(BackendMode::LlvmNative),
        "hybrid" => Some(BackendMode::Hybrid),
        _ => None,
    }
}

/// Maps a manifest build target to the CLI device model.
fn manifest_device(target: &str) -> Option<Device> {
    match target {
        "host" => Some(Device::Host),
        "wasm32" => Some(Device::Web(WasmDevice::Wasm32)),
        "wasm64" => Some(Device::Web(WasmDevice::Wasm64)),
        _ => None,
    }
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

/// Compiles a path and returns runnable IR for callers without package defaults.
fn runnable_path_ir(verb: &str, path: &str) -> Result<IrProgram, i32> {
    runnable_ir(verb, verified(path)?)
}

/// Returns a compiled program's IR, refusing a library by name.
///
/// The refusal for every verb that *starts* a program. A library has no
/// entrypoint by construction, so there is nothing to start — said plainly,
/// with the reason, rather than by failing somewhere further down where the
/// missing entrypoint looks like a compiler fault.
fn runnable_ir(verb: &str, compiled: Compiled) -> Result<IrProgram, i32> {
    let ir = compiled.ir;
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
    let resolved = resolve_path(path)?;
    kira_build::compile(std::path::Path::new(&resolved)).map_err(|error| {
        eprintln!("kirac: {error}");
        // A path the user typed that is not there is a usage error; everything
        // else got far enough that the invocation itself was fine.
        match error {
            FrontendError::Read { .. } => EXIT_USAGE,
            FrontendError::SourceMapFull { .. }
            | FrontendError::Discovery(_)
            | FrontendError::Resolution(_) => EXIT_FAILURE,
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
