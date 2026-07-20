//! Resolved package graph model.

use kira_diagnostics::Diagnostic;
use std::path::PathBuf;

/// One package admitted from a readable `package.kira` manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedPackage {
    /// The package name declared by its manifest.
    pub name: String,
    /// The package's top-level module namespace.
    pub module_root: String,
    /// The canonical directory containing `package.kira`.
    pub root_dir: PathBuf,
    /// The directory containing the package's Kira source files.
    pub source_dir: PathBuf,
    /// Dependency names declared by this package, in manifest order.
    pub dependencies: Vec<String>,
}

/// The packages reached from one root manifest and all resolution diagnostics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedPackageGraph {
    /// The root package followed by transitively resolved path dependencies.
    pub packages: Vec<ResolvedPackage>,
    /// Non-fatal problems found while resolving the graph.
    pub diagnostics: Vec<Diagnostic>,
}
