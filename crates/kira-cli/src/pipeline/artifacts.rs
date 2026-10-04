//! The verbs that produce an artifact: `build`, `package`, and `export`.
//!
//! One program, three ways of being finished with it. `build` writes whatever
//! the selected backend's artifact is; `package` is that build with the
//! precondition that an application cannot be published as a library; `export`
//! is that build with the precondition that a library actually exposes an
//! embedding surface.

use kira_backend_api::BackendMode;

use super::execute::build_native;
use super::{
    EXIT_FAILURE, EXIT_OK, apply_manifest_defaults, command_inputs, foreign_link, options_target,
    parse_options, refuse_unsupported_sanitizer, resolve_foreign, resolve_path, verified,
};
use crate::progress::{err, out};
use crate::{hybrid, hybrid_library, library, native, native_library, wasm};

/// Runs `kira build [--backend vm|llvm|hybrid] [--device host|wasm32|wasm64]
/// [--target <arch-os-abi>] <file|dir>`: compile to artifacts under
/// `.kira-build/`, without executing anything.
///
/// `--target` is the verb's cross-compilation flag, and `build` is the verb that
/// produces something with it: a program emitted for another machine is one this
/// one cannot run or debug, so the artifact is the whole of what can be done
/// here. (`check` takes the same flag and analyses against that machine's
/// native-library rows, which needs no artifact at all.) The artifacts go under
/// `.kira-build/<toolchain-triple>/` rather than beside the host's, so both
/// builds of a program can exist at once.
///
/// Two more flags travel with it, and both are about the machine rather than the
/// program: `--sysroot <dir>` names where that machine's C library and headers
/// are (falling back to the `KIRA_SYSROOT` environment variable), and
/// `--relocation-model pic|static` chooses between an ordinary
/// position-independent executable and the absolutely-addressed, non-PIE image a
/// userland with no dynamic loader needs.
pub fn build(args: &[String]) -> i32 {
    let surface = crate::progress::Surface::install("Building");
    let _guard = crate::progress::Finish(surface);
    let mut options = match parse_options("build", args) {
        Ok(options) => options,
        Err(code) => return code,
    };
    crate::diagnostics::show_notes(options.show_notes);
    let _timings = crate::timings::Timings::install(options.timings);
    options.path = match resolve_path(&options.path) {
        Ok(path) => path,
        Err(code) => return code,
    };
    // Before the frontend runs, not after it succeeds. Every failure from here
    // on — a parse error in a dependency, a link that cannot resolve a symbol —
    // ends this function early, and each one used to leave the PREVIOUS build's
    // executable sitting in `.kira-build` for someone to launch by hand and
    // mistake for this one. See `Artifacts::discard_runnable`.
    discard_stale_program(&options);
    let compiled = match verified(&options.path, &options_target(&options)) {
        Ok(compiled) => compiled,
        Err(code) => return code,
    };
    if let Err(code) = apply_manifest_defaults("build", &mut options, &compiled) {
        return code;
    }
    if let Err(code) = refuse_unsupported_sanitizer("build", &options) {
        return code;
    }
    // Again, because the manifest is what may have named another machine: the
    // first call cleared this host's layout, and a cross build's artifacts live
    // under a directory of the target's own.
    discard_stale_program(&options);
    let ir = &compiled.ir;
    // A library and a program are built by different paths on every backend:
    // one produces something a consumer depends on, the other something the OS
    // can start.
    let is_library = ir.main.is_none();

    // Resolve foreign imports once, then thread the selected target's inputs into
    // whichever artifact path owns the build.
    let foreign = match resolve_foreign(&options.path, ir, &options.device) {
        Ok(foreign) => foreign,
        Err(_) => {
            out!("Failed to build");
            return EXIT_FAILURE;
        }
    };
    let link = foreign_link(&foreign);

    if let Some(device) = options.device.wasm() {
        let source = std::path::Path::new(&options.path);
        let result = if is_library {
            wasm::build_library(ir, source, device, link, compiled.package_name.as_deref())
        } else {
            wasm::build(ir, source, device, link)
        };
        return match result {
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
            // it is the whole build. Static foreign archives are retained by
            // the explicit thin carrier when direct bindings are resolved.
            kira_diagnostics::progress!("compiling bytecode");
            match kira_bytecode::compile(ir) {
                Ok(module) => {
                    if !ir.foreign_imports.is_empty()
                        && let Err(error) = native::direct_foreign_bindings(
                            ir,
                            std::path::Path::new(&options.path),
                            link,
                        )
                    {
                        err!("kira: {error}");
                        out!("Failed to build");
                        return EXIT_FAILURE;
                    }
                    // Persist the compiled module beside the artifacts produced
                    // by the other backends.
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
                &super::native_build_target(&options),
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
                stage_bundled_assets(std::path::Path::new(&options.path), &compiled);
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
        BackendMode::Hybrid => {
            // The bundle is what a session loads; the standalone executable is
            // what the operating system starts. Both are this build's product:
            // a hybrid build that left nothing runnable would be the one
            // backend whose `.kira-build` did not hold a program.
            if let Err(error) = hybrid::build(
                ir,
                std::path::Path::new(&options.path),
                options.emit_llvm_ir,
                options.sanitize,
                link,
            ) {
                err!("kira: {error}");
                out!("Failed to build");
                return EXIT_FAILURE;
            }
            let artifacts = match native::Artifacts::for_source(std::path::Path::new(&options.path))
            {
                Ok(artifacts) => artifacts,
                Err(error) => {
                    err!("kira: {error}");
                    out!("Failed to build");
                    return EXIT_FAILURE;
                }
            };
            if let Err(error) = crate::hybrid_launcher::stage(&artifacts.executable()) {
                err!("kira: {error}");
                out!("Failed to build");
                return EXIT_FAILURE;
            }
            stage_bundled_assets(std::path::Path::new(&options.path), &compiled);
            out!("Successfully built {}", artifacts.executable().display());
            EXIT_OK
        }
    }
}

/// Stages every participating package's declared `assets` into a per-package
/// `<name>.klbundle/resources/` under the program's build directory.
///
/// A dependency's assets do not otherwise reach the program that runs: the
/// program is the only process on the machine, and it looks for a file beside
/// itself, not inside a library it was built against. So the build lays each
/// package's declared assets into a bundle named after that package, in the
/// program's own `.kira-build/`, where the runtime's file layer finds them
/// (see `kira_runtime_abi::file_system` path resolution). A KCoreUI catalog a
/// toolkit ships reaches the app this way without the app restating it.
///
/// Best-effort: an asset a manifest names but that is not on disk was already
/// reported by the frontend, and a copy that fails leaves the working-directory
/// path as the only place to find it — the same place it was before.
pub(super) fn stage_bundled_assets(entry: &std::path::Path, compiled: &kira_build::Compiled) {
    let build_directory = kira_project::build_directory(entry);

    // The distinct package roots that took part in this build, the entry's own
    // among them. A root appears under many source files; it is bundled once.
    let mut roots: Vec<std::path::PathBuf> = Vec::new();
    for file in compiled.sources.iter() {
        if let Some(root) = kira_project::package_root(std::path::Path::new(&file.path))
            && !roots.contains(&root)
        {
            roots.push(root);
        }
    }

    for root in roots {
        // Only the declaration manifest (`package.kira`) carries an `assets`
        // key; a package written in another manifest form simply has none to
        // stage, so a parse that does not yield one is skipped, not an error.
        let Some(manifest_path) = kira_project::MANIFEST_FILE_NAMES
            .iter()
            .map(|name| root.join(name))
            .find(|path| path.is_file())
        else {
            continue;
        };
        let Ok(text) = std::fs::read_to_string(&manifest_path) else {
            continue;
        };
        let Ok(manifest) = kira_manifest::load_declaration(&text) else {
            continue;
        };
        if manifest.assets.is_empty() {
            continue;
        }

        let resources = build_directory
            .join(format!("{}.klbundle", manifest.name))
            .join("resources");
        for entry in &manifest.assets {
            let relative = entry.trim_start_matches("./").trim_matches('/');
            if relative.is_empty() {
                continue;
            }
            let source = root.join(relative);
            if !source.is_file() {
                continue;
            }
            let destination = resources.join(relative);
            if let Some(parent) = destination.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let _ = std::fs::copy(&source, &destination);
        }
    }
}

/// Discards the runnable artifacts the last build of `options.path` left.
///
/// Best-effort by design: a program that has never been built has nothing to
/// discard, and a build directory that cannot be opened is a build that is
/// about to fail on its own terms with a better message than this could give.
fn discard_stale_program(options: &crate::options::CompileOptions) {
    let source = std::path::Path::new(&options.path);
    let target = super::native_build_target(options);
    if let Ok(artifacts) = native::Artifacts::for_source_targeting(source, &target) {
        artifacts.discard_runnable(&target);
    }
}

/// Runs `kira package`: the distribution-facing library build.
///
/// Library artifacts already contain the complete consumer contract — the VM
/// bytecode plus wrapper crate, the LLVM archive plus wrapper, or the hybrid
/// bundle plus wrapper. This verb adds the important precondition that an
/// application cannot accidentally be published as a package, then delegates
/// the artifact work to the same build paths `kira build` uses.
pub fn package(args: &[String]) -> i32 {
    let (options, compiled) = match command_inputs("package", args) {
        Ok(inputs) => inputs,
        Err(code) => return code,
    };
    if compiled.ir.main.is_some() {
        err!(
            "kira package: `{}` is an application, not a library; \
             set `let kind = .Library` and provide a consumer-facing package",
            options.path
        );
        return EXIT_FAILURE;
    }
    build(args)
}
