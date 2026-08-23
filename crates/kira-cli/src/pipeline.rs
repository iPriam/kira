//! What every compiling verb shares: options, discovery, the frontend, and the
//! target each of them compiles for.
//!
//! The verbs themselves live beside this: the ones that start a program in
//! [`commands`], the ones that produce an artifact in [`artifacts`], the engine
//! each of them runs on in [`execute`], and `lint` in [`lint`].
//!
//! All of them resolve a `.kira` file or package directory, drive the salsa
//! frontend to collect diagnostics, and render any errors readably against the source. `check`
//! stops there. `run` and `build` continue into the backend `--backend`
//! selects, on the device `--device` selects.
//!
//! `--device` is an override. On the host, `--backend` picks the engine; a
//! Web device has exactly one code generator, so naming the device decides
//! the backend, and a differing `--backend` beside it is overridden aloud —
//! never served, never silently swapped. `--target <arch-os-abi>` is the third
//! way of naming a machine and behaves the same way: it is the LLVM backend or
//! nothing, because the interpreter runs bytecode here and not on an aarch64
//! box somewhere else.
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

use kira_backend_api::{BackendMode, WasmDevice};
use kira_build::{Compiled, FrontendError};
use kira_diagnostics::Diagnostic;
use kira_ir::IrProgram;
use kira_llvm_backend::{NativeBuildTarget, NativeLinkInputs};
use kira_source::SourceMap;

use crate::options::{CompileOptions, Device};
use crate::progress::err;

mod artifacts;
mod commands;
mod execute;
mod lint;

pub use artifacts::{build, export, package};
pub use commands::{check, debug, live, run, test};
/// The interpreter's system-call gate, for the commands that start a VM outside
/// this module. `debug` builds its own VM target, so it has to ask the same
/// question `run` and `test` ask — and say the same thing about the answer.
pub(crate) use execute::{syscall_refusal, unservable_syscalls};
pub use lint::lint;

/// Process exit code for a clean run.
pub const EXIT_OK: i32 = 0;
/// Process exit code for compile errors, runtime traps, or bad usage.
pub const EXIT_FAILURE: i32 = 1;
/// Process exit code for usage errors (missing arguments, unreadable file).
pub const EXIT_USAGE: i32 = 2;

/// Resolves the program's `@FFI.Extern` imports to link inputs for `device`'s
/// target, reporting a resolution failure as an exit code.
///
/// `None` when the program declares no foreign imports.
fn resolve_foreign(
    source: &str,
    ir: &IrProgram,
    device: &Device,
) -> Result<Option<NativeLinkInputs>, i32> {
    let target = crate::foreign_libs::target_for_device(device);
    crate::foreign_libs::resolve(std::path::Path::new(source), ir, target).map_err(|error| {
        err!("kira: {error}");
        EXIT_FAILURE
    })
}

/// What a native build for `options` emits and links for.
///
/// The one place the command line's `--target`, `--sysroot`, and
/// `--relocation-model` become the value the backend reads, so codegen and the
/// link cannot be aimed at two different machines by two different callers.
pub(crate) fn native_build_target(options: &CompileOptions) -> NativeBuildTarget {
    NativeBuildTarget::new(options.device.native_target(), options.sysroot.clone())
}

/// The link inputs a backend uses, empty when there are no foreign imports.
fn foreign_link(foreign: &Option<NativeLinkInputs>) -> &NativeLinkInputs {
    foreign.as_ref().unwrap_or(&EMPTY_FOREIGN_LINK)
}

/// The inputs a program with no foreign imports links: nothing at all.
static EMPTY_FOREIGN_LINK: NativeLinkInputs = NativeLinkInputs::EMPTY;

/// Parses `args`, resolves the package they name, compiles it, and settles the
/// backend and device the manifest asked for.
///
/// The four steps every verb that compiles a named program takes, in the one
/// order that works: the target is decided before the frontend runs, and the
/// manifest's defaults are applied after it, because they are read out of the
/// program it produced.
pub(crate) fn command_inputs(
    verb: &str,
    args: &[String],
) -> Result<(CompileOptions, Compiled), i32> {
    let mut options = parse_options(verb, args)?;
    options.path = resolve_path(&options.path)?;
    let compiled = verified(&options.path, &options_target(&options))?;
    apply_manifest_defaults(verb, &mut options, &compiled)?;
    Ok((options, compiled))
}

/// A compiled program's IR, refusing a library that has no entrypoint.
pub(crate) fn entrypoint_ir(verb: &str, compiled: Compiled) -> Result<IrProgram, i32> {
    runnable_ir(verb, compiled)
}

/// The link inputs a program's `@FFI.Extern` imports resolve to.
pub(crate) fn foreign_inputs(
    source: &str,
    ir: &IrProgram,
    device: &Device,
) -> Result<Option<NativeLinkInputs>, i32> {
    resolve_foreign(source, ir, device)
}

/// The link inputs to pass a backend, empty when there are no foreign imports.
pub(crate) fn foreign_link_of(foreign: &Option<NativeLinkInputs>) -> &NativeLinkInputs {
    foreign_link(foreign)
}

/// Parses shared options, reporting usage errors against `verb`.
fn parse_options(verb: &str, args: &[String]) -> Result<CompileOptions, i32> {
    let options = CompileOptions::parse(args).map_err(|error| {
        err!("kira {verb}: {error}");
        EXIT_USAGE
    })?;
    if options.quit_after.is_some() && verb != "run" {
        err!("kira {verb}: `--quit-after` bounds a running program, and `{verb}` does not run one");
        return Err(EXIT_USAGE);
    }
    Ok(options)
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
            | kira_project::DiscoveryError::Malformed { .. }
            | kira_project::DiscoveryError::LegacyMalformed { .. } => EXIT_FAILURE,
        }
    })?;
    target.source_path.ok_or_else(|| {
        err!("kira: `{path}` did not resolve to a compilable Kira source");
        EXIT_USAGE
    })
}

/// Package-command access to the same discovery decision the build pipeline uses.
pub(crate) fn resolve_source_path(path: &str) -> Result<String, i32> {
    resolve_path(path)
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
    explicit: Option<&Device>,
) -> kira_native_lib_definition::TargetTriple {
    if let Some(device) = explicit {
        return crate::foreign_libs::target_for_device(device);
    }
    let declared = kira_project::manifest_for(std::path::Path::new(path))
        .ok()
        .flatten()
        .map(|found| found.manifest.build_target)
        .and_then(|target| manifest_device(&target));
    crate::foreign_libs::target_for_device(&declared.unwrap_or(Device::Host))
}

/// [`compile_target`] for a verb that has already parsed its options.
fn options_target(options: &CompileOptions) -> kira_native_lib_definition::TargetTriple {
    compile_target(
        &options.path,
        options.device_explicit.then_some(&options.device),
    )
}

/// The target selected before compiling a command's source.
pub(crate) fn compile_target_for_options(
    options: &CompileOptions,
) -> kira_native_lib_definition::TargetTriple {
    options_target(options)
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

    // The machine is settled now — from the command line or from the manifest —
    // so this is where the relocation model and the linkage can be attached to
    // it, and where one that has nothing to attach to is refused. Refused rather
    // than dropped: a freestanding userland asking for `static` and silently
    // getting a position-independent image is a program that links and does not
    // start.
    options.device = options
        .device
        .with_link_settings(options.relocation, options.linkage)
        .ok_or_else(|| {
            err!(
                "kira {verb}: {}",
                crate::options::OptionsError::LinkSettingWithoutTarget {
                    setting: if options.relocation.is_some() {
                        "--relocation-model"
                    } else {
                        "--linkage"
                    },
                    device: options.device.to_string(),
                }
            );
            EXIT_USAGE
        })?;

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
                "kira: compiling for `{}` overrides `--backend {}`: a build that is \
                 not for this machine's interpreter has one code generator",
                options.device,
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
///
/// The three device names, and then any `arch-os-abi` triple, so a package that
/// exists to be built for one machine — an operating system's userland, say —
/// says so once in its manifest instead of on every command line. It is the same
/// spelling `--target` takes and the same one a `NativeLibrary` row is keyed by,
/// which is what keeps `buildTarget = "aarch64-linux-gnu"` and
/// `[target.aarch64-linux-gnu]` meaning one thing.
///
/// A manifest cannot ask for a relocation model: that is a property of the image
/// a particular build wants, not of the package, and `--relocation-model` is
/// where it is asked for.
fn manifest_device(target: &str) -> Option<Device> {
    match target {
        "host" => Some(Device::Host),
        "wasm32" => Some(Device::Web(WasmDevice::Wasm32)),
        "wasm64" => Some(Device::Web(WasmDevice::Wasm64)),
        triple => kira_native_lib_definition::TargetTriple::parse(triple)
            .ok()
            .map(|triple| {
                Device::Cross(kira_backend_api::CrossTarget::new(
                    triple,
                    kira_backend_api::RelocationModel::default(),
                    kira_backend_api::Linkage::default(),
                ))
            }),
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

/// Compiles an already-discovered source and renders its diagnostics.
pub(crate) fn compile_verified_path(
    path: &str,
    target: &kira_native_lib_definition::TargetTriple,
) -> Result<Compiled, i32> {
    verified(path, target)
}

fn verified_with_frontend(
    path: &str,
    target: &kira_native_lib_definition::TargetTriple,
    frontend: &mut kira_build::FrontendSession,
) -> Result<Compiled, i32> {
    let compiled = compile_with_frontend(path, target, frontend)?;
    emit_diagnostics(&compiled.diagnostics, &compiled.sources);
    if compiled.has_errors() {
        return Err(EXIT_FAILURE);
    }
    Ok(compiled)
}

/// Compiles a live path while retaining the source map that defines its watch
/// roots. The caller still applies the runnable-program check after it has
/// collected those roots.
pub(crate) fn runnable_path_compiled_with_frontend(
    path: &str,
    target: &kira_native_lib_definition::TargetTriple,
    frontend: &mut kira_build::FrontendSession,
) -> Result<Compiled, i32> {
    verified_with_frontend(path, target, frontend)
}

/// Returns a compiled program's IR, refusing a library by name.
///
/// The refusal for every verb that *starts* a program. A library has no
/// entrypoint by construction, so there is nothing to start — said plainly,
/// with the reason, rather than by failing somewhere further down where the
/// missing entrypoint looks like a compiler fault.
pub(crate) fn runnable_ir(verb: &str, compiled: Compiled) -> Result<IrProgram, i32> {
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
    let mut frontend = kira_build::FrontendSession::new();
    compile_with_frontend(path, target, &mut frontend)
}

fn compile_with_frontend(
    path: &str,
    target: &kira_native_lib_definition::TargetTriple,
    frontend: &mut kira_build::FrontendSession,
) -> Result<Compiled, i32> {
    let resolved = resolve_path(path)?;
    frontend
        .compile_for(std::path::Path::new(&resolved), None, target)
        .map_err(|error| {
            err!("kira: {error}");
            // A path the user typed that is not there is a usage error; everything
            // else got far enough that the invocation itself was fine.
            match error {
                // A file that could not be read is the same usage error whether it
                // is the path the user typed or a source the package claims to own.
                FrontendError::Read { .. }
                | FrontendError::Assembly(kira_program_graph::AssemblyError::Read { .. }) => {
                    EXIT_USAGE
                }
                FrontendError::SourceLimit { .. } | FrontendError::Assembly(_) => EXIT_FAILURE,
            }
        })
}

/// Renders every diagnostic to stderr in source order.
fn emit_diagnostics(diagnostics: &[Diagnostic], sources: &SourceMap) {
    crate::diagnostics::emit(diagnostics, sources);
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
            compile_target(path, Some(&Device::Web(WasmDevice::Wasm32))).to_string(),
            "wasm32-emscripten-unknown"
        );
        // No manifest above it and no explicit device: this machine.
        assert_eq!(
            compile_target(path, None),
            crate::foreign_libs::target_for_device(&Device::Host)
        );
    }

    /// A cross target selects native-library rows by the triple that was asked
    /// for, which is what makes a package's `[target.aarch64-linux-gnu]` row the
    /// one an aarch64 build links.
    #[test]
    fn a_cross_target_selects_that_machines_native_library_rows() {
        let path = "/nonexistent/kira/x.kira";
        let device = manifest_device("aarch64-linux-gnu").expect("a triple is a build target");
        assert_eq!(
            compile_target(path, Some(&device)).to_string(),
            "aarch64-linux-gnu"
        );
        // The three device names keep meaning what they meant.
        assert_eq!(manifest_device("host"), Some(Device::Host));
        assert_eq!(
            manifest_device("wasm32"),
            Some(Device::Web(WasmDevice::Wasm32))
        );
        assert_eq!(manifest_device("aarch64-linux"), None);
    }

    /// A package whose manifest names the machine still gets to choose how its
    /// image is addressed, which is what a userland with no dynamic loader
    /// needs — and a build for this machine is refused rather than quietly
    /// handed a position-independent image it did not ask for.
    #[test]
    fn a_relocation_model_attaches_to_a_manifest_supplied_target() {
        let device = manifest_device("aarch64-linux-gnu").expect("a triple is a build target");
        let with_static = device
            .with_link_settings(Some(kira_backend_api::RelocationModel::Static), None)
            .expect("a cross device takes a relocation model");
        let Device::Cross(target) = &with_static else {
            panic!("a cross device stays one");
        };
        assert_eq!(
            target.relocation(),
            kira_backend_api::RelocationModel::Static
        );
        assert_eq!(target.triple().to_string(), "aarch64-linux-gnu");

        assert_eq!(
            Device::Host.with_link_settings(Some(kira_backend_api::RelocationModel::Static), None),
            None
        );
        assert_eq!(
            Device::Host.with_link_settings(None, None),
            Some(Device::Host)
        );
    }

    /// The linkage reaches a manifest-supplied target the same way, and setting
    /// one leaves the other alone: a package that names `buildTarget` and a
    /// command line that asks only for `--linkage static` must not silently lose
    /// the relocation model, or gain one.
    #[test]
    fn a_linkage_attaches_without_disturbing_the_relocation_model() {
        let device = manifest_device("aarch64-linux-gnu").expect("a triple is a build target");
        let freestanding = device
            .with_link_settings(
                Some(kira_backend_api::RelocationModel::Static),
                Some(kira_backend_api::Linkage::Static),
            )
            .expect("a cross device takes both");
        let Device::Cross(target) = &freestanding else {
            panic!("a cross device stays one");
        };
        assert_eq!(
            target.relocation(),
            kira_backend_api::RelocationModel::Static
        );
        assert_eq!(target.linkage(), kira_backend_api::Linkage::Static);

        let only_linkage = freestanding
            .with_link_settings(None, Some(kira_backend_api::Linkage::Dynamic))
            .expect("a cross device takes a linkage alone");
        let Device::Cross(target) = &only_linkage else {
            panic!("a cross device stays one");
        };
        assert_eq!(
            target.relocation(),
            kira_backend_api::RelocationModel::Static,
            "setting the linkage alone must not reset the relocation model"
        );
        assert_eq!(target.linkage(), kira_backend_api::Linkage::Dynamic);

        assert_eq!(
            Device::Host.with_link_settings(None, Some(kira_backend_api::Linkage::Static)),
            None
        );
    }
}
