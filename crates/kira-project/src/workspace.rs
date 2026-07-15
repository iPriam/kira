//! Workspace model: a root path plus its loaded project.

use crate::project::Project;

/// A loaded workspace: the project rooted at `root_path`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Workspace {
    pub root_path: String,
    pub project: Project,
}
