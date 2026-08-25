//! Platform project generators for `kira export`.
//!
//! Each family — Apple (Xcode), Windows and Linux (CMake), Web — is emitted as a
//! tree of [`ExportedFile`]s: a relative path and the text that belongs at it.
//! The generators are pure text, so they are unit-tested against their own output
//! and carry no build, filesystem, or host-tool dependency. The CLI writes the
//! tree and drives whatever host build (Xcode, emscripten) the family needs on
//! top of it.

use std::path::{Path, PathBuf};

pub mod apple;
pub mod cmake;
pub mod web;

/// One file a generator emits: where it goes, relative to the family's export
/// root, and what it contains.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportedFile {
    /// The path relative to the export root, e.g. `Sources/main.m`.
    pub path: PathBuf,
    /// The file's full text.
    pub contents: String,
}

impl ExportedFile {
    /// Builds an exported file from a relative path and its text.
    pub fn new(path: impl Into<PathBuf>, contents: impl Into<String>) -> Self {
        ExportedFile {
            path: path.into(),
            contents: contents.into(),
        }
    }
}

/// A generated project: the files that make it up, ready to write under a root.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct GeneratedProject {
    /// Every file the project is made of, in emission order.
    pub files: Vec<ExportedFile>,
}

impl GeneratedProject {
    /// Writes every file under `root`, creating parent directories as needed.
    ///
    /// A relative path in an [`ExportedFile`] is joined onto `root`; the file's
    /// own directory is created first so a caller never has to sequence the two.
    pub fn write_to(&self, root: &Path) -> Result<(), ExportError> {
        for file in &self.files {
            let path = root.join(&file.path);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).map_err(|source| ExportError::Write {
                    path: parent.display().to_string(),
                    source,
                })?;
            }
            std::fs::write(&path, &file.contents).map_err(|source| ExportError::Write {
                path: path.display().to_string(),
                source,
            })?;
        }
        Ok(())
    }
}

/// Why a project could not be generated or written.
#[derive(Debug, thiserror::Error)]
pub enum ExportError {
    /// A generated file could not be written.
    #[error("cannot write `{path}`: {source}")]
    Write {
        /// The path that could not be written.
        path: String,
        /// The underlying I/O failure.
        #[source]
        source: std::io::Error,
    },
}

/// Lowercases a project name into a C/CMake identifier, mapping every character
/// that is not alphanumeric to `_`.
///
/// A CMake `project()` name and target identifier must be a bare token, so a
/// display name like `Harmony Browser` becomes `harmony_browser` rather than
/// something a generator has to quote.
pub fn safe_identifier(name: &str) -> String {
    name.chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_display_name_becomes_a_bare_identifier() {
        assert_eq!(safe_identifier("Harmony Browser"), "harmony_browser");
        assert_eq!(safe_identifier("app-123"), "app_123");
    }

    #[test]
    fn writing_a_project_lays_files_out_under_the_root() {
        let dir = std::env::temp_dir().join(format!("kira-export-write-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let project = GeneratedProject {
            files: vec![
                ExportedFile::new("CMakeLists.txt", "cmake"),
                ExportedFile::new("src/main.c", "int main(void){return 0;}"),
            ],
        };
        project.write_to(&dir).expect("the project writes");
        assert_eq!(
            std::fs::read_to_string(dir.join("src/main.c")).expect("nested file"),
            "int main(void){return 0;}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
