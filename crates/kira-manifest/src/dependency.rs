//! Dependency specifications: registry, path, and git sources.

/// A single dependency entry in a `package.kira` manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DependencySpec {
    pub name: String,
    pub source: DependencySource,
}

/// Where a dependency comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DependencySource {
    Registry(RegistrySource),
    Path(PathSource),
    Git(GitSource),
}

/// A registry dependency pinned by version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegistrySource {
    pub version: String,
}

/// A local path dependency.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathSource {
    pub path: String,
}

/// A git dependency, optionally pinned by rev or tag.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitSource {
    pub url: String,
    pub rev: Option<String>,
    pub tag: Option<String>,
}
