//! The toolchain `kira` grants a program it runs.
//!
//! The sibling of [`crate::compiler_host`]. That one answers a question about
//! source held in memory; this one answers a question about a project on a disk
//! — a directory with a `package.kira` in it — because `kira` is the process
//! that owns a real toolchain: a frontend, the backends `--backend` selects, a
//! linker, and somewhere to put what they produce.
//!
//! A Kira program calling `kcRun` is therefore not shelling out to `kira run`.
//! It reaches this installed [`Toolchain`], which drives the very pipeline the
//! command line drives — [`crate::pipeline`] — so a package builds and runs one
//! way whether a person or a program asked. The diagnostics come back as
//! values; the exit code is the one the verb produced.
//!
//! An embedder that never calls [`grant`] keeps the refusing default, and a
//! program that reaches for the toolchain there is told by name that this host
//! has none — never answered with an empty diagnostic list, which reads as a
//! package that compiled.

use kira_diagnostics::{Diagnostic, Severity};
use kira_runtime_abi::{
    CheckSeverity, ToolAnswer, ToolBackend, ToolDiagnostic, ToolRequest, ToolVariable, ToolVerb,
    Toolchain,
};
use kira_source::SourceMap;

use crate::options::Device;
use crate::pipeline;

/// The toolchain `kira` installs: the command-line pipeline, driven by value.
struct KiracToolchain;

impl Toolchain for KiracToolchain {
    fn perform(&mut self, verb: ToolVerb, request: &ToolRequest) -> ToolAnswer {
        // The diagnostics are the program's own, harvested once as values. A
        // path the frontend cannot even reach — a missing directory, an
        // unusable manifest — has none to give, and the exit code the verb
        // carries says what happened instead.
        let diagnostics = match pipeline::compiled_for(&request.path, Some(&device_for(request))) {
            Ok(compiled) => convert(&compiled.diagnostics, &compiled.sources),
            Err(_) => Vec::new(),
        };
        let exit_code = i64::from(perform_verb(verb, request, &diagnostics));
        ToolAnswer {
            exit_code,
            diagnostics,
        }
    }
}

/// Runs the verb for its exit code and its side effects.
///
/// `check` needs no more than the diagnostics already in hand — its exit code
/// is whether any of them is an error. `build` and `run` continue into the
/// backend, so they drive the same pipeline the command line does, under an
/// environment the request named.
fn perform_verb(verb: ToolVerb, request: &ToolRequest, diagnostics: &[ToolDiagnostic]) -> i32 {
    let args = command_line(request);
    match verb {
        ToolVerb::Check => {
            if diagnostics.iter().any(is_error) {
                pipeline::EXIT_FAILURE
            } else {
                pipeline::EXIT_OK
            }
        }
        ToolVerb::Build => with_environment(&request.environment, || pipeline::build(&args)),
        ToolVerb::Run => with_environment(&request.environment, || pipeline::run(&args)),
    }
}

/// The command line the request stands for: the path, then the backend flag.
///
/// The one translation from the value a program handed in to the arguments the
/// pipeline already knows how to parse, so the two paths cannot drift.
fn command_line(request: &ToolRequest) -> Vec<String> {
    vec![
        request.path.clone(),
        "--backend".to_owned(),
        backend_flag(request.backend).to_owned(),
    ]
}

/// The `--backend` value a toolchain backend selects.
///
/// A hybrid bias picks the same backend either way: the split itself is what
/// the bias steers, downstream of naming the backend, so both hybrid requests
/// name `hybrid` here.
fn backend_flag(backend: ToolBackend) -> &'static str {
    match backend {
        ToolBackend::Vm => "vm",
        ToolBackend::Native => "llvm",
        ToolBackend::HybridRuntimeBias | ToolBackend::HybridNativeBias => "hybrid",
    }
}

/// The device a request compiles for.
///
/// This host: the toolchain capability names a backend, not a machine, and a
/// program asking to build for another machine names it in the manifest the
/// path points at, exactly as a command line leaves it to the manifest.
fn device_for(_request: &ToolRequest) -> Device {
    Device::Host
}

/// Runs `body` with `environment` applied to the process, then restored.
///
/// The engines run a program in this process or in a child that inherits its
/// environment, so a variable a run is asked to expose is a variable on the
/// process while it runs. Restored after, so one run cannot leak into the next.
fn with_environment<R>(environment: &[ToolVariable], body: impl FnOnce() -> R) -> R {
    let saved: Vec<(String, Option<String>)> = environment
        .iter()
        .map(|variable| (variable.name.clone(), std::env::var(&variable.name).ok()))
        .collect();
    for variable in environment {
        // SAFETY: the run is single-threaded with respect to its own
        // environment — the verbs below start the program and wait for it — so
        // no other thread reads these while they are set.
        unsafe {
            std::env::set_var(&variable.name, &variable.value);
        }
    }
    let result = body();
    for (name, previous) in saved {
        // SAFETY: as above; this restores what was read a moment earlier.
        unsafe {
            match previous {
                Some(value) => std::env::set_var(&name, value),
                None => std::env::remove_var(&name),
            }
        }
    }
    result
}

/// Turns the frontend's diagnostics into the values the wire carries.
fn convert(diagnostics: &[Diagnostic], sources: &SourceMap) -> Vec<ToolDiagnostic> {
    diagnostics
        .iter()
        .map(|diagnostic| ToolDiagnostic {
            code: diagnostic
                .code
                .as_ref()
                .map(|code| code.as_str().to_owned())
                .unwrap_or_default(),
            line: line_of(diagnostic, sources),
            severity: severity_of(diagnostic.severity),
        })
        .collect()
}

/// The 1-based line a diagnostic points at, or `0` when it points at no source.
fn line_of(diagnostic: &Diagnostic, sources: &SourceMap) -> i64 {
    let Some(span) = diagnostic.primary_span() else {
        return 0;
    };
    i64::from(
        sources
            .get(span.source)
            .line_map
            .line_column(span.span.start)
            .line,
    )
}

/// The wire severity a frontend severity maps to.
fn severity_of(severity: Severity) -> CheckSeverity {
    match severity {
        Severity::Error => CheckSeverity::Error,
        Severity::Warning => CheckSeverity::Warning,
        Severity::Note => CheckSeverity::Note,
    }
}

/// Whether a converted diagnostic is one that stops a build.
fn is_error(diagnostic: &ToolDiagnostic) -> bool {
    diagnostic.severity == CheckSeverity::Error
}

/// Grants every host in this process the toolchain `kira` drives.
pub fn grant() {
    kira_runtime_abi::toolchain::install(Box::new(KiracToolchain));
}
