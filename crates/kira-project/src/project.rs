//! Project model types: projects, resolved roots, and build targets.

use kira_manifest::{PackageKind, ProjectManifest};

/// What a CLI command intends to do with a target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandMode {
    Check,
    Build,
    Run,
    Live,
}

/// The kind of target a path resolved to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetKind {
    Library,
    Executable,
    Example,
    SourceFile,
}

/// A loaded project (its manifest).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Project {
    pub manifest: ProjectManifest,
}

/// A project resolved from a manifest on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedProject {
    pub root_path: String,
    pub manifest_path: String,
    pub entrypoint_path: String,
    pub project: Project,
}

/// A package root resolved from a manifest on disk (libraries may have no
/// entrypoint).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedPackageRoot {
    pub root_path: String,
    pub manifest_path: String,
    pub entrypoint_path: Option<String>,
    pub module_source_root: String,
    pub project: Project,
}

/// The result of resolving an arbitrary CLI path argument to a buildable
/// target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedTarget {
    pub root_path: Option<String>,
    pub manifest_path: Option<String>,
    pub source_path: Option<String>,
    pub source_root: Option<String>,
    pub project_name: Option<String>,
    pub project: Option<Project>,
    pub package_kind: Option<PackageKind>,
    pub target_kind: TargetKind,
}

impl ResolvedTarget {
    pub fn kind_name(&self) -> &'static str {
        match self.target_kind {
            TargetKind::Library => "library",
            TargetKind::Executable => "executable",
            TargetKind::Example => "example",
            TargetKind::SourceFile => "source_file",
        }
    }

    pub fn display_path(&self) -> &str {
        self.root_path
            .as_deref()
            .or(self.source_path.as_deref())
            .unwrap_or(".")
    }

    pub fn can_check(&self) -> bool {
        true
    }

    pub fn can_build(&self) -> bool {
        true
    }

    pub fn can_run(&self) -> bool {
        match self.target_kind {
            TargetKind::Library => false,
            TargetKind::Executable | TargetKind::Example | TargetKind::SourceFile => {
                self.source_path.is_some()
            }
        }
    }

    pub fn can_live(&self) -> bool {
        match self.target_kind {
            TargetKind::Executable | TargetKind::Example => self.source_path.is_some(),
            TargetKind::Library | TargetKind::SourceFile => false,
        }
    }
}
