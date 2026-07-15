//! Non-root package manifest model.

use crate::dependency::DependencySpec;
use crate::project_manifest::PackageKind;

/// The manifest of a non-root package participating in a project graph.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageManifest {
    pub name: String,
    pub version: String,
    pub kind: PackageKind,
    pub kira_version: String,
    pub module_root: Option<String>,
    pub dependencies: Vec<DependencySpec>,
    pub native_libs: Vec<String>,
}

impl PackageManifest {
    /// A manifest with the standard field defaults.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            version: "0.1.0".to_string(),
            kind: PackageKind::Library,
            kira_version: "0.1.0".to_string(),
            module_root: None,
            dependencies: Vec::new(),
            native_libs: Vec::new(),
        }
    }
}
