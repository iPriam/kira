//! `kira.lock` model: the resolved, reproducible dependency graph.
//!
//! Note: `kira.lock` is never tracked in git.

use crate::dependency::DependencySource;

/// The lock file written next to a `package.kira` manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LockFile {
    pub schema_version: u32,
    pub root: LockRoot,
    pub packages: Vec<LockedPackage>,
}

impl Default for LockFile {
    fn default() -> Self {
        Self {
            schema_version: 1,
            root: LockRoot::default(),
            packages: Vec::new(),
        }
    }
}

/// The root package the lock file was resolved for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LockRoot {
    pub name: String,
    pub version: String,
    pub kind: String,
    pub kira_version: String,
    pub dependencies: Vec<LockRootDependency>,
}

impl Default for LockRoot {
    fn default() -> Self {
        Self {
            name: String::new(),
            version: String::new(),
            kind: "app".to_string(),
            kira_version: "0.1.0".to_string(),
            dependencies: Vec::new(),
        }
    }
}

/// A direct dependency of the root package, as declared.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LockRootDependency {
    pub name: String,
    pub source: DependencySource,
}

/// A fully resolved package pinned in the lock file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LockedPackage {
    pub name: String,
    pub version: String,
    pub kind: String,
    pub kira_version: String,
    pub module_root: String,
    pub source: LockedSource,
    pub dependencies: Vec<String>,
}

/// The pinned source a locked package was fetched from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LockedSource {
    Registry(LockedRegistrySource),
    Path(LockedPathSource),
    Git(LockedGitSource),
}

/// A registry archive pinned by checksum.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LockedRegistrySource {
    pub registry_url: String,
    pub archive_path: String,
    pub checksum: String,
}

/// A local path source (not content-pinned).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LockedPathSource {
    pub path: String,
}

/// A git source pinned to an exact commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LockedGitSource {
    pub url: String,
    pub commit: String,
    pub requested_rev: Option<String>,
    pub requested_tag: Option<String>,
}
