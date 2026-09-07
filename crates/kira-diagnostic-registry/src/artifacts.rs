//! The files the code table is written to, and whether they are current.

use std::fs;
use std::path::{Path, PathBuf};

use crate::RegistryError;
use crate::render;

/// One generated file: where it belongs and what the table says it holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Artifact {
    /// The path, relative to the repository root.
    pub path: PathBuf,
    /// The whole file, as the table renders it.
    pub contents: String,
}

impl Artifact {
    /// The file as it stands on disk, or `None` when it is not there.
    fn current(&self, repo: &Path) -> Result<Option<String>, RegistryError> {
        let path = repo.join(&self.path);
        match fs::read_to_string(&path) {
            Ok(text) => Ok(Some(text)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(RegistryError::Unreadable {
                path,
                reason: error.to_string(),
            }),
        }
    }

    /// Whether the file on disk is what the table says it holds.
    pub fn is_current(&self, repo: &Path) -> Result<bool, RegistryError> {
        Ok(self.current(repo)?.as_deref() == Some(self.contents.as_str()))
    }

    /// Writes the file, reporting whether it changed.
    pub fn write(&self, repo: &Path) -> Result<bool, RegistryError> {
        if self.is_current(repo)? {
            return Ok(false);
        }
        let path = repo.join(&self.path);
        fs::write(&path, &self.contents)
            .map(|()| true)
            .map_err(|error| RegistryError::Unwritable {
                path,
                reason: error.to_string(),
            })
    }
}

/// Every file the code table is written to.
#[must_use]
pub fn artifacts() -> Vec<Artifact> {
    vec![
        Artifact {
            path: PathBuf::from("foundation/app/Kira/Diagnostics.kira"),
            contents: render::kira_enum(),
        },
        Artifact {
            path: PathBuf::from("foundation/app/Kira/DiagnosticCodes.kira"),
            contents: render::kira_from_code(),
        },
        Artifact {
            path: PathBuf::from("sites/docs/content/docs/appendix/diagnostics/codes.mdx"),
            contents: render::docs_index(),
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::artifacts;

    #[test]
    fn every_artifact_has_a_path_and_contents() {
        let written = artifacts();
        assert_eq!(written.len(), 3);
        for artifact in written {
            assert!(artifact.path.is_relative());
            assert!(!artifact.contents.is_empty());
            assert!(artifact.contents.ends_with('\n'));
        }
    }
}
