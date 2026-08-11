//! Template-driven project generation.

use std::fs;
use std::path::{Path, PathBuf};

use kira_core::sanitize_kira_identifier;
use thiserror::Error;

/// Why a project could not be generated.
#[derive(Debug, Error)]
pub enum GenerationError {
    /// The requested destination is an existing file.
    #[error("project destination `{path}` is not a directory")]
    DestinationNotDirectory {
        /// The path that was requested.
        path: PathBuf,
    },
    /// Generation refuses to overwrite an existing project.
    #[error("project destination `{path}` is not empty")]
    DestinationNotEmpty {
        /// The path that was requested.
        path: PathBuf,
    },
    /// The supplied name cannot become a Kira package identifier.
    #[error("project name `{name}` is empty")]
    InvalidName {
        /// The name supplied by the caller.
        name: String,
    },
    /// Reading the destination directory failed.
    #[error("cannot inspect project destination `{path}`: {source}")]
    InspectDestination {
        /// The path that could not be inspected.
        path: PathBuf,
        /// The filesystem failure.
        source: std::io::Error,
    },
    /// Creating the destination directory failed.
    #[error("cannot create project destination `{path}`: {source}")]
    CreateDestination {
        /// The path that could not be created.
        path: PathBuf,
        /// The filesystem failure.
        source: std::io::Error,
    },
    /// Writing one generated file failed.
    #[error("cannot write generated file `{path}`: {source}")]
    WriteFile {
        /// The file that could not be written.
        path: PathBuf,
        /// The filesystem failure.
        source: std::io::Error,
    },
}

/// Which template `kira new` instantiates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TemplateKind {
    App,
    Library,
}

impl TemplateKind {
    /// The directory name of the template under the templates root.
    pub fn template_dir_name(self) -> &'static str {
        match self {
            Self::App => "app",
            Self::Library => "library",
        }
    }

    /// The name shown by the CLI for this template.
    pub fn label(self) -> &'static str {
        match self {
            Self::App => "app",
            Self::Library => "library",
        }
    }

    /// Generate a new project in an empty `root` directory.
    ///
    /// The package name is sanitized with the same identifier rule used by
    /// generated bindings. Existing non-empty directories are never touched.
    pub fn generate(
        self,
        root: &Path,
        package_name: &str,
    ) -> Result<GeneratedProject, GenerationError> {
        if package_name.trim().is_empty() {
            return Err(GenerationError::InvalidName {
                name: package_name.to_owned(),
            });
        }
        let name = sanitize_kira_identifier(package_name, "App");
        prepare_destination(root)?;

        let app = root.join("app");
        fs::create_dir_all(&app).map_err(|source| GenerationError::CreateDestination {
            path: app.clone(),
            source,
        })?;

        let source_name = match self {
            Self::App => "main.kira".to_owned(),
            Self::Library => format!("{name}.kira"),
        };
        let kind = match self {
            Self::App => ".App",
            Self::Library => ".Library",
        };
        let files = [
            (
                root.join("package.kira"),
                format!(
                    "Package {name} {{\n    let version = \"0.1.0\"\n    let kind = {kind}\n}}\n"
                ),
            ),
            (app.join(&source_name), source(self, &name)),
            (root.join("README.md"), readme(self, &name)),
        ];

        let mut written = Vec::with_capacity(files.len());
        for (path, text) in files {
            if let Err(source) = fs::write(&path, text) {
                for previous in &written {
                    let _ = fs::remove_file(previous);
                }
                let _ = fs::remove_dir(&app);
                return Err(GenerationError::WriteFile { path, source });
            }
            written.push(path);
        }
        Ok(GeneratedProject {
            root: root.to_path_buf(),
            files: written,
        })
    }
}

/// The files successfully written by one generation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GeneratedProject {
    /// The destination directory.
    pub root: PathBuf,
    /// Every generated file, in write order.
    pub files: Vec<PathBuf>,
}

/// Ensure generation will not overwrite a project that already has content.
fn prepare_destination(root: &Path) -> Result<(), GenerationError> {
    match fs::metadata(root) {
        Ok(metadata) if !metadata.is_dir() => Err(GenerationError::DestinationNotDirectory {
            path: root.to_path_buf(),
        }),
        Ok(_) => {
            let mut entries =
                fs::read_dir(root).map_err(|source| GenerationError::InspectDestination {
                    path: root.to_path_buf(),
                    source,
                })?;
            if entries
                .next()
                .transpose()
                .map_err(|source| GenerationError::InspectDestination {
                    path: root.to_path_buf(),
                    source,
                })?
                .is_some()
            {
                return Err(GenerationError::DestinationNotEmpty {
                    path: root.to_path_buf(),
                });
            }
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => fs::create_dir_all(root)
            .map_err(|source| GenerationError::CreateDestination {
                path: root.to_path_buf(),
                source,
            }),
        Err(source) => Err(GenerationError::InspectDestination {
            path: root.to_path_buf(),
            source,
        }),
    }
}

/// Content of the source entrypoint for a template.
fn source(kind: TemplateKind, name: &str) -> String {
    match kind {
        TemplateKind::App => {
            format!("@Main function main() {{\n    print(\"hello from {name}\")\n    return\n}}\n")
        }
        TemplateKind::Library => format!(
            "@Export function greeting() -> String {{\n    return \"hello from {name}\"\n}}\n"
        ),
    }
}

/// Content of the generated project's orientation file.
fn readme(kind: TemplateKind, name: &str) -> String {
    let command = match kind {
        TemplateKind::App => "run",
        TemplateKind::Library => "build",
    };
    let option = if kind == TemplateKind::Library {
        " --library"
    } else {
        ""
    };
    format!(
        "# {name}\n\nGenerated with `kira new{option}`.\n\nRun `kira {command} .` from this directory.\n"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temporary_root(label: &str) -> PathBuf {
        let root =
            std::env::temp_dir().join(format!("kira_generation_{}_{}", std::process::id(), label));
        let _ = fs::remove_dir_all(&root);
        root
    }

    #[test]
    fn app_generation_writes_a_checkable_package() {
        let root = temporary_root("app");
        let generated = TemplateKind::App
            .generate(&root, "hello-world")
            .expect("generates");

        assert_eq!(generated.files.len(), 3);
        assert!(root.join("package.kira").is_file());
        assert!(root.join("app/main.kira").is_file());
        assert!(
            fs::read_to_string(root.join("package.kira"))
                .expect("manifest")
                .contains("Package hello_world")
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn generation_refuses_to_overwrite_content() {
        let root = temporary_root("existing");
        fs::create_dir_all(&root).expect("directory");
        fs::write(root.join("keep.txt"), "keep").expect("content");

        assert!(matches!(
            TemplateKind::Library.generate(&root, "library"),
            Err(GenerationError::DestinationNotEmpty { .. })
        ));
        assert!(root.join("keep.txt").is_file());
        let _ = fs::remove_dir_all(root);
    }
}
