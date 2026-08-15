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
    parse_options, resolve_foreign, resolve_path, verified,
};
use crate::options::Device;
use crate::progress::{err, out};
use crate::{hybrid, hybrid_library, library, native, native_library, wasm};

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
    crate::diagnostics::show_notes(options.show_notes);
    let _timings = crate::timings::Timings::install(options.timings);
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
    // A library and a program are built by different paths on every backend:
    // one produces something a consumer depends on, the other something the OS
    // can start.
    let is_library = ir.main.is_none();

    // Resolve foreign imports once, then thread the selected target's inputs into
    // whichever artifact path owns the build.
    let foreign = match resolve_foreign(&options.path, ir, options.device) {
        Ok(foreign) => foreign,
        Err(_) => {
            out!("Failed to build");
            return EXIT_FAILURE;
        }
    };
    let link = foreign_link(&foreign);

    if let Device::Web(device) = options.device {
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

/// Runs `kira export`: build a library that actually exposes an embedding API.
pub fn export(args: &[String]) -> i32 {
    let (options, compiled) = match command_inputs("export", args) {
        Ok(inputs) => inputs,
        Err(code) => return code,
    };
    if compiled.ir.main.is_some() {
        err!(
            "kira export: `{}` is an application, not a library",
            options.path
        );
        return EXIT_FAILURE;
    }
    if compiled.ir.exports.is_empty() {
        err!(
            "kira export: `{}` declares no `@Export` functions; add at least one \
             consumer-facing export before building the wrapper",
            options.path
        );
        return EXIT_FAILURE;
    }
    build(args)
}
