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
use kira_diagnostics::{Diagnostic, Suggestion, renderer};
use kira_ir::IrProgram;
use kira_llvm_backend::NativeLinkInputs;
use kira_source::{SourceId, SourceMap};

use kira_backend_api::WasmDevice;

use crate::hybrid;
use crate::hybrid_library;
use crate::library;
use crate::native;
use crate::native_library;
use crate::options::{CompileOptions, Device};
use crate::progress::{err, out};
use crate::wasm;

mod execute;

use self::execute::{build_native, run_hybrid, run_native, run_on_vm, run_web};

/// Process exit code for a clean run.
pub const EXIT_OK: i32 = 0;
/// Process exit code for compile errors, runtime traps, or bad usage.
pub const EXIT_FAILURE: i32 = 1;
/// Process exit code for usage errors (missing arguments, unreadable file).
pub const EXIT_USAGE: i32 = 2;

/// Runs `kira check [file|dir]`: report diagnostics, never execute.
///
/// With no path, checks the package you are standing in — the same default
/// `run` and `build` take.
pub fn check(args: &[String]) -> i32 {
    let surface = crate::progress::Surface::install("Checking");
    let _guard = crate::progress::Finish(surface);
    let path = args
        .first()
        .map(String::as_str)
        .unwrap_or(crate::options::DEFAULT_PATH);
    match compile(path, &compile_target(path, None)) {
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

/// Runs `kira lint <file|dir>`: report what the package's lints found.
///
/// Closer to `check` than to `test`. A lint runs during *expansion* — the
/// `LintRunner` collector is handed every declaration and reports as it goes —
/// so there is nothing to execute afterwards and no backend to pick. Compiling
/// the package is the whole of the work; this only decides what to print and
/// what to exit with.
///
/// A lint that warns does not fail the run, because a warning is an opinion
/// about code that already compiles. Only an error does, which is what a
/// `linter.kira` entry asks for when it writes `severity = "error"`.
pub fn lint(args: &[String]) -> i32 {
    let surface = crate::progress::Surface::install("Linting");
    let _guard = crate::progress::Finish(surface);
    let apply = args.iter().any(|arg| arg == "--fix");
    let path = args
        .iter()
        .find(|arg| !arg.starts_with("--"))
        .map(String::as_str)
        .unwrap_or(crate::options::DEFAULT_PATH);
    // Set before anything is compiled, because the frontend reads it once at the
    // edge and turns it into a salsa input. This is the whole of what tells the
    // lint runner it was asked for; every other verb leaves it unset and the
    // runner returns without looking at a single declaration.
    //
    // SAFETY: single-threaded, before any thread that could read the
    // environment is started — the compile below is the first thing that does.
    unsafe { std::env::set_var(kira_build::frontend::LINT_MODE, "1") };
    match compile(path, &compile_target(path, None)) {
        Ok(compiled) => {
            // Only what lives under the path being linted. A collector is
            // handed every declaration in the *program*, dependencies included,
            // so a lint configured here would otherwise report against
            // Foundation and every library — findings the reader cannot act on
            // because they do not own the code.
            //
            // Scoping covers findings only. An *error* is kept wherever it was
            // raised, because an error outside the linted path is not a finding
            // the reader cannot act on — it is the run failing. A lint whose own
            // runner would not evaluate reported nothing for exactly this
            // reason, under a printed `ok`, which is the shape of a fake
            // success: silence read as a clean bill of health.
            let owned: Vec<Diagnostic> = compiled
                .diagnostics
                .iter()
                .filter(|diagnostic| {
                    diagnostic.severity == kira_diagnostics::Severity::Error
                        // The receipt is the runner talking about itself, so it
                        // is anchored in the runner — outside the linted path,
                        // every time. Scoping it away is what made a run that
                        // never happened look like a run that found nothing.
                        || diagnostic.has_code(RECEIPT)
                        || under(path, diagnostic, &compiled.sources)
                })
                .cloned()
                .collect();
            // The runner's receipt, taken out of the findings before anything is
            // printed: it says how many lints ran, which is not something a
            // reader wants listed as a finding.
            let ran = lints_that_ran(&owned);
            let owned: Vec<Diagnostic> = owned
                .into_iter()
                .filter(|diagnostic| !diagnostic.has_code(RECEIPT))
                .collect();
            emit_diagnostics(&owned, &compiled.sources);
            if kira_diagnostics::has_errors(&owned) {
                return EXIT_FAILURE;
            }
            let reported = owned.len();
            if apply {
                match apply_fixes(&owned, &compiled.sources) {
                    Ok(0) => out!("ok: {path} — nothing to fix"),
                    Ok(count) => {
                        out!("ok: {path} — applied {count} fix(es); run again to re-check")
                    }
                    Err(reason) => {
                        out!("kira lint: {reason}");
                        return EXIT_FAILURE;
                    }
                }
                return EXIT_OK;
            }
            // Silence is only good news when something was listening. Without
            // the receipt the runner did not run — no `linter.kira`, or one that
            // failed before it could report — and saying "clean" would be a
            // lie, so this fails instead.
            match ran {
                None => {
                    out!(
                        "kira lint: {path} — the lint runner did not run, so nothing was checked. \
                         Add a `linter.kira` beside `package.kira`, or read the errors above."
                    );
                    EXIT_FAILURE
                }
                // The runner ran and had nothing to run. Worded without naming a
                // file, because this is equally what a package with no
                // `linter.kira` gets and what one whose entries are all
                // `enabled = false` gets — and claiming a file exists that
                // does not is the same class of lie as claiming a clean run.
                Some(0) => {
                    out!(
                        "ok: {path} — no lint is enabled, so nothing was checked. \
                         Enable one in `linter.kira` beside `package.kira`."
                    );
                    EXIT_OK
                }
                Some(count) if reported == 0 => {
                    out!("ok: {path} — {count} lint(s) ran, nothing found");
                    EXIT_OK
                }
                Some(count) => {
                    out!("ok: {path} — {reported} report(s) from {count} lint(s)");
                    EXIT_OK
                }
            }
        }
        Err(code) => code,
    }
}

/// The code Foundation's lint runner reports its own arrival under.
///
/// Not a finding: it is how the runner says it ran, and how many lints it ran,
/// so silence can be told from absence. `kira lint` consumes it.
const RECEIPT: &str = "KLINT000";

/// How many lints ran, or `None` when the runner never reported.
///
/// The count is the trailing number of `lints ran: N`. A receipt that cannot be
/// read counts as no receipt: a run that cannot say what it checked has not
/// earned the word "clean".
fn lints_that_ran(diagnostics: &[Diagnostic]) -> Option<usize> {
    let receipt = diagnostics
        .iter()
        .find(|diagnostic| diagnostic.has_code(RECEIPT))?;
    // `{"lintsRan":N}`. Read by hand rather than through a JSON crate: it is one
    // field the runner and this function agree on, and the agreement is pinned
    // by a test either way.
    let count = receipt.message.split_once(':')?.1;
    count.trim_end_matches('}').trim().parse().ok()
}

/// Whether a diagnostic points inside the directory being linted.
///
/// Compared by canonical path so a relative `.` and an absolute root name the
/// same tree. A diagnostic with no span belongs to nobody in particular and is
/// kept, because dropping it would hide a whole-program complaint.
fn under(root: &str, diagnostic: &Diagnostic, sources: &SourceMap) -> bool {
    let Some(span) = diagnostic.primary_span() else {
        return true;
    };
    let index = span.source.value() as usize;
    if index >= sources.len() {
        return true;
    }
    let file = std::path::Path::new(&sources.get(span.source).path).to_path_buf();
    let root = std::path::Path::new(root);
    match (file.canonicalize(), root.canonicalize()) {
        (Ok(file), Ok(root)) => file.starts_with(root),
        // An unreadable path cannot be placed, and guessing would either hide a
        // real finding or invent one.
        _ => true,
    }
}

/// Writes every machine-applicable suggestion back to its file.
///
/// Back to front within each file, so an earlier edit never moves the span a
/// later one was measured against. Only `MachineApplicable` is written: anything
/// less is a suggestion for a reader, and applying it unattended is how a tool
/// silently changes what a program means.
fn apply_fixes(diagnostics: &[Diagnostic], sources: &SourceMap) -> Result<usize, String> {
    let mut per_file: std::collections::BTreeMap<usize, Vec<&Suggestion>> =
        std::collections::BTreeMap::new();
    for diagnostic in diagnostics {
        let Some(suggestion) = &diagnostic.suggestion else {
            continue;
        };
        if !suggestion.is_machine_applicable() {
            continue;
        }
        per_file
            .entry(suggestion.span.source.value() as usize)
            .or_default()
            .push(suggestion);
    }

    let mut applied = 0;
    for (index, mut fixes) in per_file {
        if index >= sources.len() {
            continue;
        }
        let file = sources.get(SourceId::new(index as u32));
        let mut text = file.text.clone();
        // Descending by start, so each write leaves every earlier span intact.
        fixes.sort_by_key(|fix| std::cmp::Reverse(fix.span.span.start));
        for fix in fixes {
            let start = fix.span.span.start as usize;
            let end = fix.span.span.end() as usize;
            if end > text.len() || start > end {
                return Err(format!(
                    "a fix for `{}` names bytes {start}..{end}, which the file does not have",
                    file.path
                ));
            }
            text.replace_range(start..end, &fix.replacement);
            applied += 1;
        }
        std::fs::write(&file.path, text)
            .map_err(|error| format!("`{}` could not be written: {error}", file.path))?;
    }
    Ok(applied)
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

    let foreign = match resolve_foreign(&options.path, &ir, options.device) {
        Ok(foreign) => foreign,
        Err(code) => return code,
    };
    let link = foreign_link(&foreign);

    if let Device::Web(device) = options.device {
        return run_web(&ir, &options, device, link);
    }

    match options.backend {
        BackendMode::VmBytecode => run_on_vm(&ir, std::path::Path::new(&options.path), link),
        BackendMode::LlvmNative => run_native(&ir, &options, link),
        BackendMode::Hybrid => run_hybrid(&ir, &options, link),
    }
}

/// The function a `kira test` run enters.
///
/// Generated by Foundation's `TestRunner` collector macro, which is ordinary
/// Kira: nothing in this compiler knows what a `Test` is, and this name is the
/// whole of the agreement between the two sides.
const TEST_ENTRY: &str = "kiraTestMain";

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
                 note: a test is a `Test` declaration, and `import Foundation` is what \
                 brings the family and its runner into a package"
            );
            return EXIT_FAILURE;
        }
    }

    let foreign = match resolve_foreign(&options.path, &ir, options.device) {
        Ok(foreign) => foreign,
        Err(code) => return code,
    };
    let link = foreign_link(&foreign);

    if let Device::Web(device) = options.device {
        return run_web(&ir, &options, device, link);
    }

    match options.backend {
        BackendMode::VmBytecode => run_on_vm(&ir, std::path::Path::new(&options.path), link),
        BackendMode::LlvmNative => run_native(&ir, &options, link),
        BackendMode::Hybrid => run_hybrid(&ir, &options, link),
    }
}

/// Resolves the program's `@FFI.Extern` imports to link inputs for `device`'s
/// target, reporting a resolution failure as an exit code.
///
/// `None` when the program declares no foreign imports.
fn resolve_foreign(
    source: &str,
    ir: &IrProgram,
    device: Device,
) -> Result<Option<NativeLinkInputs>, i32> {
    let target = crate::foreign_libs::target_for_device(device);
    crate::foreign_libs::resolve(std::path::Path::new(source), ir, target).map_err(|error| {
        err!("kira: {error}");
        EXIT_FAILURE
    })
}

/// The link inputs a backend uses, empty when there are no foreign imports.
fn foreign_link(foreign: &Option<NativeLinkInputs>) -> &NativeLinkInputs {
    foreign.as_ref().unwrap_or(&EMPTY_FOREIGN_LINK)
}

/// The inputs a program with no foreign imports links: nothing at all.
static EMPTY_FOREIGN_LINK: NativeLinkInputs = NativeLinkInputs::EMPTY;

/// Runs `kira live [runner] [file|dir] [--backend vm|hybrid]`: build a bundle,
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
    // Two paths, because a package is a tree and a build has one entry. The
    // watched path is what the user named — for a package, the whole directory,
    // so a save anywhere in `app/` reloads. The source is the entry package
    // discovery resolves it to, which is what names the build artifacts.
    let watched = std::path::PathBuf::from(&options.path);
    let entry = match resolve_path(&options.path) {
        Ok(entry) => entry,
        Err(code) => return code,
    };
    let source = std::path::Path::new(&entry);

    // Compiling is a closure rather than a value, because a watched session
    // rebuilds: the frontend runs again for every save, and a save that does not
    // compile yields `None` rather than an error, so the session keeps the app
    // that is already running.
    let rebuild = || -> Result<Option<kira_live::Bundle>, crate::live::LiveError> {
        let Ok(ir) = runnable_path_ir(
            "live",
            &entry,
            &crate::foreign_libs::target_for_device(Device::Host),
        ) else {
            return Ok(None);
        };
        // A live session runs on the machine the runner runs on, so the foreign
        // libraries it links are the host's — resolved on every rebuild, because
        // a save can add an import that needs one. A resolution failure is a
        // failed build like any other: the session keeps the app it is running
        // and the diagnostic is already on stderr.
        let Ok(foreign) = resolve_foreign(&entry, &ir, Device::Host) else {
            return Ok(None);
        };
        crate::live::build_bundle(
            &ir,
            source,
            options.runner,
            options.backend,
            foreign_link(&foreign),
        )
        .map(Some)
    };

    match crate::supervisor::run(&options, &watched, &rebuild) {
        Ok(()) => EXIT_OK,
        Err(error) => {
            err!("kira live: {error}");
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
    err!(
        "kira {verb}: `--backend {}` on `--device {}`: library export is not built yet: \
         {missing}\n\
         note: this package exports {}\n\
         note: `--backend vm` builds this library's export surface today, into \
         `.kira-build/rust/<package>/`",
        backend.label(),
        device.label(),
        names.join(", "),
    );
    out!("Failed to {verb}");
    Err(EXIT_FAILURE)
}

/// Runs `kira build [--backend vm|llvm|hybrid] [--device host|wasm32|wasm64]
/// <file|dir>`: compile to artifacts under `.kira-build/`, without executing
/// anything.
pub fn build(args: &[String]) -> i32 {
    let surface = crate::progress::Surface::install("Building");
    let _guard = crate::progress::Finish(surface);
    let mut options = match parse_options("build", args) {
        Ok(options) => options,
        Err(code) => return code,
    };
    options.path = match resolve_path(&options.path) {
        Ok(path) => path,
        Err(code) => return code,
    };
    let compiled = match verified(&options.path, &options_target(&options)) {
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

    // Resolve the program's foreign imports to link inputs for the selected target
    // once, and thread them into whichever backend runs. A library's foreign
    // surface is not built in this milestone, so only program arms link them.
    let foreign = match resolve_foreign(&options.path, ir, options.device) {
        Ok(foreign) => foreign,
        Err(_) => {
            out!("Failed to build");
            return EXIT_FAILURE;
        }
    };
    let link = foreign_link(&foreign);

    if let Device::Web(device) = options.device {
        return match wasm::build(ir, std::path::Path::new(&options.path), device, link) {
            Ok(artifacts) => {
                out!("Successfully built {}", artifacts.wasm.display());
                EXIT_OK
            }
            Err(error) => {
                err!("kira: {error}");
                out!("Failed to build");
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
                    err!("kira: {error}");
                    out!("Failed to build");
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
                Ok(module) => {
                    if !ir.foreign_imports.is_empty()
                        && let Err(error) = native::build_adapter_sidecar(
                            ir,
                            std::path::Path::new(&options.path),
                            link,
                        )
                    {
                        err!("kira: {error}");
                        out!("Failed to build");
                        return EXIT_FAILURE;
                    }
                    // The module the compile just produced, written where every
                    // other backend writes. It used to be dropped on the floor:
                    // the match bound `Ok(_)`, nothing reached the disk, and
                    // the command still said it had built something — so `kira
                    // build` on the default backend reported success and left
                    // the directory exactly as it found it.
                    let artifacts =
                        match native::Artifacts::for_source(std::path::Path::new(&options.path)) {
                            Ok(artifacts) => artifacts,
                            Err(error) => {
                                err!("kira: {error}");
                                out!("Failed to build");
                                return EXIT_FAILURE;
                            }
                        };
                    let bytecode = artifacts.bytecode();
                    if let Err(error) = std::fs::write(&bytecode, module.to_bytes()) {
                        err!("kira: cannot write {}: {error}", bytecode.display());
                        out!("Failed to build");
                        return EXIT_FAILURE;
                    }
                    out!("Successfully built {}", bytecode.display());
                    EXIT_OK
                }
                Err(error) => {
                    err!("kira: bytecode compilation failed: {error}");
                    out!("Failed to build");
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
                    err!("kira: {error}");
                    out!("Failed to build");
                    EXIT_FAILURE
                }
            }
        }
        BackendMode::LlvmNative => match build_native(ir, &options, link) {
            Some(_) => {
                out!("Successfully built");
                EXIT_OK
            }
            None => {
                out!("Failed to build");
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
                    err!("kira: {error}");
                    out!("Failed to build");
                    EXIT_FAILURE
                }
            }
        }
        BackendMode::Hybrid => match hybrid::build(
            ir,
            std::path::Path::new(&options.path),
            options.emit_llvm_ir,
            link,
        ) {
            Ok(_) => {
                out!("Successfully built");
                EXIT_OK
            }
            Err(error) => {
                err!("kira: {error}");
                out!("Failed to build");
                EXIT_FAILURE
            }
        },
    }
}

/// Parses shared options, reporting usage errors against `verb`.
fn parse_options(verb: &str, args: &[String]) -> Result<CompileOptions, i32> {
    CompileOptions::parse(args).map_err(|error| {
        err!("kira {verb}: {error}");
        EXIT_USAGE
    })
}

/// Resolves a package directory to the source file that seeds compilation.
fn resolve_path(path: &str) -> Result<String, i32> {
    let target = kira_project::resolve_target(std::path::Path::new(path)).map_err(|error| {
        err!("kira: {error}");
        match error {
            kira_project::DiscoveryError::NotPackageDirectory { .. }
            | kira_project::DiscoveryError::MissingEntrypoint { .. }
            | kira_project::DiscoveryError::NoLibrarySources { .. } => EXIT_USAGE,
            kira_project::DiscoveryError::Unreadable { .. }
            | kira_project::DiscoveryError::Malformed { .. } => EXIT_FAILURE,
        }
    })?;
    target.source_path.ok_or_else(|| {
        err!("kira: `{path}` did not resolve to a compilable Kira source");
        EXIT_USAGE
    })
}

/// The target a build for `path` selects, decided before the program compiles.
///
/// Autobind runs inside the frontend and generates per target, so the target
/// cannot wait for [`apply_manifest_defaults`] — that reads the compiled
/// program, which is already too late. An explicit `--device` wins; otherwise
/// the manifest's own `buildTarget` decides, read straight off the manifest;
/// otherwise this host.
fn compile_target(
    path: &str,
    explicit: Option<Device>,
) -> kira_native_lib_definition::TargetTriple {
    if let Some(device) = explicit {
        return crate::foreign_libs::target_for_device(device);
    }
    let declared = kira_project::manifest_for(std::path::Path::new(path))
        .ok()
        .flatten()
        .map(|found| found.manifest.build_target)
        .and_then(|target| manifest_device(&target));
    crate::foreign_libs::target_for_device(declared.unwrap_or(Device::Host))
}

/// [`compile_target`] for a verb that has already parsed its options.
fn options_target(options: &CompileOptions) -> kira_native_lib_definition::TargetTriple {
    compile_target(
        &options.path,
        options.device_explicit.then_some(options.device),
    )
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
            err!("kira {verb}: unknown manifest build target `{target}`");
            EXIT_FAILURE
        })?;
    }

    if matches!(options.device, Device::Host) {
        if !options.backend_explicit
            && let Some(mode) = compiled.default_execution_mode.as_deref()
        {
            options.backend = manifest_backend(mode).ok_or_else(|| {
                err!("kira {verb}: unknown manifest execution mode `{mode}`");
                EXIT_FAILURE
            })?;
        }
    } else {
        if options.backend_explicit && options.backend != BackendMode::LlvmNative {
            err!(
                "kira: `--device {}` overrides `--backend {}`: the Web device has one code generator",
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
fn verified_as(
    verb: &str,
    path: &str,
    kind: kira_semantics::BuildKind,
    target: &kira_native_lib_definition::TargetTriple,
) -> Result<Compiled, i32> {
    let resolved = resolve_path(path)?;
    let compiled = kira_build::compile_for(std::path::Path::new(&resolved), Some(kind), target)
        .map_err(|error| {
            err!("kira {verb}: {error}");
            EXIT_FAILURE
        })?;
    emit_diagnostics(&compiled.diagnostics, &compiled.sources);
    if compiled.has_errors() {
        return Err(EXIT_FAILURE);
    }
    Ok(compiled)
}

fn verified(
    path: &str,
    target: &kira_native_lib_definition::TargetTriple,
) -> Result<Compiled, i32> {
    let compiled = compile(path, target)?;
    emit_diagnostics(&compiled.diagnostics, &compiled.sources);
    if compiled.has_errors() {
        return Err(EXIT_FAILURE);
    }
    Ok(compiled)
}

/// Compiles a path and returns runnable IR for callers without package defaults.
fn runnable_path_ir(
    verb: &str,
    path: &str,
    target: &kira_native_lib_definition::TargetTriple,
) -> Result<IrProgram, i32> {
    runnable_ir(verb, verified(path, target)?)
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
        err!(
            "kira {verb}: cannot {verb} a library: a library has no `@Main` \
             entrypoint, because it is entered by whatever consumes it\n\
             note: `kira build` compiles a library to an artifact a consumer links"
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
fn compile(path: &str, target: &kira_native_lib_definition::TargetTriple) -> Result<Compiled, i32> {
    let resolved = resolve_path(path)?;
    kira_build::compile_for(std::path::Path::new(&resolved), None, target).map_err(|error| {
        err!("kira: {error}");
        // A path the user typed that is not there is a usage error; everything
        // else got far enough that the invocation itself was fine.
        match error {
            // A file that could not be read is the same usage error whether it
            // is the path the user typed or a source the package claims to own.
            FrontendError::Read { .. }
            | FrontendError::Assembly(kira_program_graph::AssemblyError::Read { .. }) => EXIT_USAGE,
            FrontendError::SourceMapFull { .. } | FrontendError::Assembly(_) => EXIT_FAILURE,
        }
    })
}

/// Renders every diagnostic to stderr in source order.
fn emit_diagnostics(diagnostics: &[Diagnostic], sources: &SourceMap) {
    // The status surface redraws in place; a diagnostic printed underneath it
    // would interleave into half a status block, a note, and a block that
    // scrolled. It stands aside and redraws on the next phase. Suspended once
    // for the whole run rather than per line, which `err!` would also do
    // correctly but at one erase check per diagnostic.
    let _surface = kira_diagnostics::progress::suspended();
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
        let path = "/nonexistent/kira/x.kira";
        assert_eq!(
            compile(path, &compile_target(path, None)).err(),
            Some(EXIT_USAGE)
        );
    }

    #[test]
    fn an_explicit_device_decides_the_target_bindings_are_generated_for() {
        let path = "/nonexistent/kira/x.kira";
        assert_eq!(
            compile_target(path, Some(Device::Web(WasmDevice::Wasm32))).to_string(),
            "wasm32-emscripten-unknown"
        );
        // No manifest above it and no explicit device: this machine.
        assert_eq!(
            compile_target(path, None),
            crate::foreign_libs::target_for_device(Device::Host)
        );
    }
}
