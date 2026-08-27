//! Stable diagnostic codes (`KIC*`, `KPK*`, `KCL*`, `KTC*`, `KBE*`, ...).
//!
//! Codes span the KIC / KIR / KBE / KTC / KPK / KCL families (plus KSEM codes
//! owned by semantics). A few representative entries are scaffolded here;
//! codes are added as each phase lands, keeping the `text()` mapping
//! exhaustive.

/// One stable, user-facing diagnostic code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DiagnosticCode {
    /// KIC001 — generic internal compiler error (approved fallback only).
    Kic001GenericInternalCompilerError,
    /// KBE001 — a backend was asked for a feature it does not support.
    Kbe001UnsupportedBackendFeature,
    /// KTC001 — the LLVM toolchain could not be found.
    Ktc001MissingLlvmToolchain,
    /// KPK001 — no project manifest was found.
    Kpk001MissingProjectManifest,
    /// KPK002 — a `package.kira` declaration could not be read.
    Kpk002InvalidPackageDeclaration,
    /// KPK007 — the target entrypoint source file is missing.
    Kpk007MissingSourceFile,
    /// KPK010 — a library target has no buildable source files.
    Kpk010NoBuildableTarget,
    /// KCL001 — unknown CLI command.
    Kcl001UnknownCommand,
    /// KPK020 — a path dependency has no readable package manifest.
    Kpk020MissingDependencyPackage,
    /// KPK021 — package dependencies contain a cycle.
    Kpk021CyclicPackageDependency,
    /// KPK022 — a manifest declares the same dependency more than once.
    Kpk022DuplicateDependencyDeclaration,
    /// KPK023 — a package name resolves to conflicting identities.
    Kpk023ConflictingPackageIdentity,
    /// KPK024 — the lockfile disagrees with the resolved manifest graph.
    Kpk024LockfileDrift,
    /// KPK025 — a `*_types.kira` source sits outside a `bind-types/` directory.
    Kpk025MisplacedBindTypesFile,
    /// KPK026 — a drifted lockfile was rewritten from the manifests.
    Kpk026LockfileSynced,
    /// KPK027 — a drifted lockfile could not be rewritten.
    Kpk027LockfileSyncFailed,
    /// KPK028 — a registry dependency has no configured fetcher.
    Kpk028RegistryDependencyUnavailable,
    /// KPK029 — a git dependency has no configured fetcher.
    Kpk029GitDependencyUnavailable,
    /// KPK030 — a package manifest handed to the compiler could not be read.
    Kpk030UnreadableManifest,
    /// KPK031 — the root a check request named is in no package it listed.
    Kpk031UnknownRootPackage,
    /// KPK032 — a dependency package's manifest exists but could not be read
    /// or parsed.
    Kpk032UnreadableDependencyManifest,
    /// KPK040 — a native library's headers could not be bound.
    Kpk040AutobindFailed,
    /// KPK041 — a binding the package ships was adopted rather than generated.
    Kpk041AutobindAdopted,
}

impl DiagnosticCode {
    /// Returns the code's user-facing string (e.g. `"KPK001"`).
    pub fn as_str(self) -> &'static str {
        match self {
            DiagnosticCode::Kic001GenericInternalCompilerError => "KIC001",
            DiagnosticCode::Kbe001UnsupportedBackendFeature => "KBE001",
            DiagnosticCode::Ktc001MissingLlvmToolchain => "KTC001",
            DiagnosticCode::Kpk001MissingProjectManifest => "KPK001",
            DiagnosticCode::Kpk002InvalidPackageDeclaration => "KPK002",
            DiagnosticCode::Kpk007MissingSourceFile => "KPK007",
            DiagnosticCode::Kpk010NoBuildableTarget => "KPK010",
            DiagnosticCode::Kcl001UnknownCommand => "KCL001",
            DiagnosticCode::Kpk020MissingDependencyPackage => "KPK020",
            DiagnosticCode::Kpk021CyclicPackageDependency => "KPK021",
            DiagnosticCode::Kpk022DuplicateDependencyDeclaration => "KPK022",
            DiagnosticCode::Kpk023ConflictingPackageIdentity => "KPK023",
            DiagnosticCode::Kpk024LockfileDrift => "KPK024",
            DiagnosticCode::Kpk025MisplacedBindTypesFile => "KPK025",
            DiagnosticCode::Kpk026LockfileSynced => "KPK026",
            DiagnosticCode::Kpk027LockfileSyncFailed => "KPK027",
            DiagnosticCode::Kpk028RegistryDependencyUnavailable => "KPK028",
            DiagnosticCode::Kpk029GitDependencyUnavailable => "KPK029",
            DiagnosticCode::Kpk030UnreadableManifest => "KPK030",
            DiagnosticCode::Kpk031UnknownRootPackage => "KPK031",
            DiagnosticCode::Kpk032UnreadableDependencyManifest => "KPK032",
            DiagnosticCode::Kpk040AutobindFailed => "KPK040",
            DiagnosticCode::Kpk041AutobindAdopted => "KPK041",
        }
    }

    /// Returns the code's user-facing text (e.g. `"KPK001"`).
    pub fn text(self) -> &'static str {
        self.as_str()
    }
}

#[cfg(test)]
mod tests {
    use super::DiagnosticCode;

    #[test]
    fn package_resolution_codes_render_stable_strings() {
        let cases = [
            (DiagnosticCode::Kpk020MissingDependencyPackage, "KPK020"),
            (DiagnosticCode::Kpk021CyclicPackageDependency, "KPK021"),
            (
                DiagnosticCode::Kpk022DuplicateDependencyDeclaration,
                "KPK022",
            ),
            (DiagnosticCode::Kpk023ConflictingPackageIdentity, "KPK023"),
            (DiagnosticCode::Kpk024LockfileDrift, "KPK024"),
            (DiagnosticCode::Kpk025MisplacedBindTypesFile, "KPK025"),
            (DiagnosticCode::Kpk026LockfileSynced, "KPK026"),
            (DiagnosticCode::Kpk027LockfileSyncFailed, "KPK027"),
            (DiagnosticCode::Kpk030UnreadableManifest, "KPK030"),
            (DiagnosticCode::Kpk031UnknownRootPackage, "KPK031"),
            (DiagnosticCode::Kpk032UnreadableDependencyManifest, "KPK032"),
        ];

        for (code, expected) in cases {
            assert_eq!(code.as_str(), expected);
        }
    }
}
