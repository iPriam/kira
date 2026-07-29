//! Resolved package graph model.

use kira_diagnostics::Diagnostic;
use kira_manifest::DependencySource;
use std::path::PathBuf;

/// One dependency edge, as the depending manifest declared it.
///
/// The name and the source travel together because a lockfile records both:
/// the graph is keyed by name, but a `[[root_dependency]]` entry has to say
/// where the dependency came from, in the spelling the manifest used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedDependency {
    /// The dependency name as declared.
    pub name: String,
    /// Where the dependency is declared to come from.
    pub source: DependencySource,
}

/// One package admitted from a readable `package.kira` manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedPackage {
    /// The package name declared by its manifest.
    pub name: String,
    /// The package version declared by its manifest.
    pub version: String,
    /// The package kind (`app` or `library`) as a lockfile spells it.
    pub kind: String,
    /// The Kira language version the manifest asks for.
    pub kira_version: String,
    /// The package's top-level module namespace.
    pub module_root: String,
    /// The canonical directory containing `package.kira`.
    pub root_dir: PathBuf,
    /// The directory containing the package's Kira source files.
    pub source_dir: PathBuf,
    /// Dependencies declared by this package, in manifest order.
    pub dependencies: Vec<ResolvedDependency>,
}

impl ResolvedPackage {
    /// The declared dependency names, in manifest order.
    pub fn dependency_names(&self) -> impl Iterator<Item = &str> {
        self.dependencies
            .iter()
            .map(|dependency| dependency.name.as_str())
    }
}

/// What a `kira.lock` beside the root manifest says about this graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LockfileStatus {
    /// No `kira.lock` sits beside the root manifest.
    Absent,
    /// The lockfile records exactly this graph.
    Current,
    /// The lockfile records a different graph than the manifests resolve to.
    Drifted,
}

/// The packages reached from one root manifest and all resolution diagnostics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedPackageGraph {
    /// The root package followed by transitively resolved path dependencies.
    pub packages: Vec<ResolvedPackage>,
    /// Non-fatal problems found while resolving the graph.
    pub diagnostics: Vec<Diagnostic>,
    /// What the lockfile beside the root manifest says about this graph.
    ///
    /// Resolution itself never writes: a caller that wants the lockfile
    /// brought back in line calls [`crate::sync_lockfile`] with this graph.
    pub lockfile: LockfileStatus,
}
