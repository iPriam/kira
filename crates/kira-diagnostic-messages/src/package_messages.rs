//! Cataloged package/project-discovery messages (`KPK*`).
//!
//! Three representative constructors are scaffolded; the remaining `KPK*`
//! messages are added as package discovery grows.

use crate::compiler_phase::CompilerPhase;
use crate::diagnostic_code::DiagnosticCode;
use crate::diagnostic_domain::DiagnosticDomain;
use crate::diagnostic_message::{MessageArgs, build};
use kira_diagnostics::{Diagnostic, Severity};

fn package_error(code: DiagnosticCode, title: &str, message: String, help: &str) -> Diagnostic {
    build(MessageArgs {
        code,
        severity: Severity::Error,
        domain: DiagnosticDomain::Package,
        phase: Some(CompilerPhase::ProjectDiscovery),
        title: title.to_owned(),
        message,
        span: None,
        label: None,
        notes: Vec::new(),
        help: Some(help.to_owned()),
    })
}

/// Builds KPK001: no `package.kira` manifest was found under `path`.
pub fn missing_project_manifest(path: &str) -> Diagnostic {
    package_error(
        DiagnosticCode::Kpk001MissingProjectManifest,
        "project manifest not found",
        format!(
            "Kira could not find `package.kira` (or the legacy `kira.toml`/`project.toml`) under `{path}`."
        ),
        "Run the command from a project root, or pass an explicit manifest path.",
    )
}

/// Builds KPK007: the target entrypoint `app/main.kira` is missing under `root`.
pub fn missing_source_file(root: &str) -> Diagnostic {
    package_error(
        DiagnosticCode::Kpk007MissingSourceFile,
        "target entrypoint is missing",
        format!(
            "Kira expected `app/main.kira` under `{root}`, but that source file does not exist."
        ),
        "Add `app/main.kira`, or point the command at a library root for `check`/`build` only.",
    )
}

/// Builds KPK010: a library target has no `.kira` sources under `source_root`.
pub fn no_buildable_target(source_root: &str) -> Diagnostic {
    package_error(
        DiagnosticCode::Kpk010NoBuildableTarget,
        "library has no source files",
        format!("Kira could not find any `.kira` source files under `{source_root}`."),
        "Add library source files under the package `app/` directory.",
    )
}
