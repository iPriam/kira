//! Cataloged package/project-discovery messages (`KPK*`).

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

fn package_warning(code: DiagnosticCode, title: &str, message: String, help: &str) -> Diagnostic {
    build(MessageArgs {
        code,
        severity: Severity::Warning,
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

/// Builds KPK020: dependency `name` has no readable manifest at `resolved_path`.
pub fn missing_dependency_package(name: &str, resolved_path: &str) -> Diagnostic {
    package_error(
        DiagnosticCode::Kpk020MissingDependencyPackage,
        "dependency package is missing",
        format!(
            "Dependency `{name}` resolves to `{resolved_path}`, but Kira could not read a `package.kira` manifest there."
        ),
        "Correct the dependency path and ensure the target package contains a readable `package.kira` manifest.",
    )
}

/// Builds KPK021: package dependencies contain the ordered `cycle`.
pub fn cyclic_package_dependency(cycle: &[String]) -> Diagnostic {
    package_error(
        DiagnosticCode::Kpk021CyclicPackageDependency,
        "package dependency cycle detected",
        format!("Package dependencies form a cycle: {}.", cycle.join(" -> ")),
        "Remove or redirect one dependency edge so the package dependency graph is acyclic.",
    )
}

/// Builds KPK022: dependency `name` is declared more than once in one manifest.
pub fn duplicate_dependency_declaration(name: &str) -> Diagnostic {
    package_warning(
        DiagnosticCode::Kpk022DuplicateDependencyDeclaration,
        "duplicate dependency declaration",
        format!("Dependency `{name}` is declared more than once in the same package manifest."),
        "Keep one declaration for this dependency name; Kira will ignore the duplicates.",
    )
}

/// Builds KPK023: `name` has two incompatible package identities.
pub fn conflicting_package_identity(
    name: &str,
    first_identity: &str,
    second_identity: &str,
) -> Diagnostic {
    package_error(
        DiagnosticCode::Kpk023ConflictingPackageIdentity,
        "conflicting package identity",
        format!(
            "Package `{name}` has conflicting identities: `{first_identity}` and `{second_identity}`."
        ),
        "Make the dependency name, declared package name, module root, and canonical package path agree.",
    )
}

/// Builds KPK024: `description` explains how the lockfile drifted from manifests.
pub fn lockfile_drift(description: &str) -> Diagnostic {
    package_warning(
        DiagnosticCode::Kpk024LockfileDrift,
        "lockfile does not match package manifests",
        format!("The resolved package graph differs from `kira.lock`: {description}."),
        "Regenerate `kira.lock` from the current package manifests; resolution will continue using the manifests.",
    )
}

/// Builds KPK025: the `*_types.kira` source at `path` is not in a `bind-types/`
/// directory.
///
/// A `*_types.kira` file is the convention for hand-authored foreign-binding
/// type vocabulary (the C primitive typedefs a generated binding assumes), and
/// it must live in a `bind-types/` directory so it stays separate from a
/// package's own `types/` domain types and from generated `bindings/`.
pub fn misplaced_bind_types_file(path: &str) -> Diagnostic {
    package_error(
        DiagnosticCode::Kpk025MisplacedBindTypesFile,
        "`*_types.kira` file outside `bind-types/`",
        format!(
            "`{path}` ends in `_types.kira` but does not sit directly in a `bind-types/` directory."
        ),
        "Move the file into a `bind-types/` directory beside `bindings/`, or rename it if it is not foreign-binding type vocabulary.",
    )
}

#[cfg(test)]
mod tests {
    use super::{
        conflicting_package_identity, cyclic_package_dependency, duplicate_dependency_declaration,
        lockfile_drift, misplaced_bind_types_file, missing_dependency_package,
    };
    use crate::{CompilerPhase, DiagnosticDomain};
    use kira_diagnostics::{Diagnostic, Severity};

    fn assert_package_diagnostic(diagnostic: &Diagnostic, code: &'static str, severity: Severity) {
        assert_eq!(diagnostic.code, Some(code));
        assert_eq!(diagnostic.severity, severity);
        assert_eq!(diagnostic.domain, Some(DiagnosticDomain::Package.tag()));
        assert_eq!(
            diagnostic.phase,
            Some(CompilerPhase::ProjectDiscovery.tag())
        );
        assert!(diagnostic.labels.is_empty());
        assert!(diagnostic.help.is_some());
    }

    #[test]
    fn missing_dependency_package_is_package_error() {
        let diagnostic = missing_dependency_package("Core", "/packages/core");

        assert_package_diagnostic(&diagnostic, "KPK020", Severity::Error);
    }

    #[test]
    fn cyclic_package_dependency_is_package_error() {
        let cycle = vec!["Core".to_owned(), "Graphics".to_owned(), "Core".to_owned()];
        let diagnostic = cyclic_package_dependency(&cycle);

        assert_package_diagnostic(&diagnostic, "KPK021", Severity::Error);
    }

    #[test]
    fn duplicate_dependency_declaration_is_package_warning() {
        let diagnostic = duplicate_dependency_declaration("Core");

        assert_package_diagnostic(&diagnostic, "KPK022", Severity::Warning);
    }

    #[test]
    fn conflicting_package_identity_is_package_error() {
        let diagnostic =
            conflicting_package_identity("Core", "/packages/core-a", "/packages/core-b");

        assert_package_diagnostic(&diagnostic, "KPK023", Severity::Error);
    }

    #[test]
    fn lockfile_drift_is_package_warning() {
        let diagnostic = lockfile_drift("package `Core` is absent");

        assert_package_diagnostic(&diagnostic, "KPK024", Severity::Warning);
    }

    #[test]
    fn misplaced_bind_types_file_is_package_error() {
        let diagnostic = misplaced_bind_types_file("app/types/vulkan_types.kira");

        assert_package_diagnostic(&diagnostic, "KPK025", Severity::Error);
    }
}
