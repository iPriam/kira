//! Package-manager model types: sync options and the resolved graph.
//!
//! Ported from kira-zig `kira_package_manager/src/types.zig`.

use kira_manifest::DependencySpec;

/// Options controlling a `kira sync` run.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SyncOptions {
    pub offline: bool,
    pub locked: bool,
    pub update_registry: bool,
    pub update_git: bool,
    pub registry_url_override: Option<String>,
}

/// The resolved dependency graph produced by a sync.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedGraph {
    pub packages: Vec<ResolvedPackage>,
}

/// One fully resolved package in the graph.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedPackage {
    pub name: String,
    pub version: String,
    pub kind: String,
    pub kira_version: String,
    pub module_root: String,
    pub source_root: String,
    pub source: ResolvedPackageSource,
    pub dependencies: Vec<DependencySpec>,
}

/// Where a resolved package's sources came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolvedPackageSource {
    Registry {
        registry_url: String,
        archive_path: String,
        checksum: String,
    },
    Path {
        path: String,
    },
    Git {
        url: String,
        commit: String,
    },
}
