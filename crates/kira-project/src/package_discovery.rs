//! Manifest discovery: file names and target resolution from paths.
//!
//! The load/resolve functions land as discovery grows; the manifest naming
//! constants are the stable surface.

use kira_manifest::{DeclarationError, PackageKind, ProjectManifest};

/// The declaration manifest. It takes precedence over `kira.toml` when both
/// are present in a package directory (it is first in
/// [`MANIFEST_FILE_NAMES`]).
pub const DECLARATION_MANIFEST_FILE_NAME: &str = "package.kira";
pub const PREFERRED_MANIFEST_FILE_NAME: &str = "kira.toml";
pub const LEGACY_MANIFEST_FILE_NAME: &str = "project.toml";
pub const REPO_MANIFEST_FILE_NAME: &str = "Kira.toml";
pub const MANIFEST_FILE_NAME: &str = PREFERRED_MANIFEST_FILE_NAME;
pub const ENTRYPOINT_REL_PATH: &str = "app/main.kira";

/// All accepted manifest file names, in precedence order.
pub const MANIFEST_FILE_NAMES: [&str; 4] = [
    DECLARATION_MANIFEST_FILE_NAME,
    PREFERRED_MANIFEST_FILE_NAME,
    LEGACY_MANIFEST_FILE_NAME,
    REPO_MANIFEST_FILE_NAME,
];

/// True when the manifest at `path` is a `package.kira` declaration manifest.
pub fn is_declaration_manifest(path: &str) -> bool {
    std::path::Path::new(path)
        .file_name()
        .is_some_and(|name| name == DECLARATION_MANIFEST_FILE_NAME)
}

/// Why the package a source file belongs to could not be determined.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DiscoveryError {
    /// A `package.kira` was found but could not be read from disk.
    #[error("cannot read `{path}`: {message}")]
    Unreadable {
        /// The manifest that could not be read.
        path: String,
        /// The underlying I/O failure, rendered.
        message: String,
    },
    /// A `package.kira` was found and is not a valid declaration.
    #[error("cannot read the package manifest `{path}`: {source}")]
    Malformed {
        /// The manifest that could not be parsed.
        path: String,
        /// Why parsing failed.
        #[source]
        source: DeclarationError,
    },
}

/// The manifest governing `source`, found by walking up from its directory.
///
/// `Ok(None)` means no `package.kira` sits above the file. That is not an
/// error: a bare `.kira` file handed to `kirac` is a program in its own right,
/// and the caller supplies the default. A manifest that *is* present and
/// unreadable is an error, because silently falling back to the default would
/// build the wrong kind of thing.
///
/// Only `package.kira` is consulted. The other names in [`MANIFEST_FILE_NAMES`]
/// are TOML forms with no loader yet; finding one and ignoring it is the same
/// silent-wrong-answer this function exists to avoid, so they are skipped
/// explicitly rather than by omission — a `kira.toml`-only package resolves to
/// `None` and is built as a program, exactly as it is today.
pub fn manifest_for(source: &std::path::Path) -> Result<Option<Manifest>, DiscoveryError> {
    let start = if source.is_dir() {
        Some(source)
    } else {
        source.parent()
    };
    let mut dir = start;
    while let Some(current) = dir {
        let candidate = current.join(DECLARATION_MANIFEST_FILE_NAME);
        if candidate.is_file() {
            let path = candidate.display().to_string();
            let text = std::fs::read_to_string(&candidate).map_err(|error| {
                DiscoveryError::Unreadable {
                    path: path.clone(),
                    message: error.to_string(),
                }
            })?;
            let manifest = kira_manifest::load_declaration(&text)
                .map_err(|source| DiscoveryError::Malformed { path, source })?;
            return Ok(Some(Manifest {
                path: candidate.display().to_string(),
                manifest,
            }));
        }
        dir = current.parent();
    }
    Ok(None)
}

/// A manifest found on disk, with the path it was read from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    /// Where the manifest was read from, for diagnostics.
    pub path: String,
    /// The manifest itself.
    pub manifest: ProjectManifest,
}

impl Manifest {
    /// The kind of package this manifest declares.
    pub fn kind(&self) -> PackageKind {
        self.manifest.kind
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scratch directory that removes itself, so a failing test leaves no
    /// litter and no test depends on another's leftovers.
    struct TempDir(std::path::PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let base = std::env::temp_dir().join(format!(
                "kira-discovery-{tag}-{}-{:?}",
                std::process::id(),
                std::thread::current().id(),
            ));
            std::fs::create_dir_all(&base).expect("a scratch directory");
            Self(base)
        }

        fn path(&self) -> &std::path::Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn a_file_with_no_manifest_above_it_resolves_to_nothing() {
        let dir = TempDir::new("none");
        let source = dir.path().join("main.kira");
        std::fs::write(&source, "@Main function main() { return }").unwrap();
        assert_eq!(manifest_for(&source), Ok(None));
    }

    #[test]
    fn a_library_manifest_beside_the_file_is_found() {
        let dir = TempDir::new("beside");
        std::fs::write(
            dir.path().join(DECLARATION_MANIFEST_FILE_NAME),
            "Package uifoundation {\n let kind = .Library\n}",
        )
        .unwrap();
        let source = dir.path().join("lib.kira");
        std::fs::write(&source, "function f() { return }").unwrap();
        let found = manifest_for(&source).unwrap().expect("a manifest");
        assert_eq!(found.kind(), PackageKind::Library);
        assert_eq!(found.manifest.name, "uifoundation");
    }

    #[test]
    fn discovery_walks_up_from_a_nested_source_directory() {
        let dir = TempDir::new("nested");
        std::fs::write(
            dir.path().join(DECLARATION_MANIFEST_FILE_NAME),
            "Package deep {\n let kind = .Library\n}",
        )
        .unwrap();
        let nested = dir.path().join("app").join("sub");
        std::fs::create_dir_all(&nested).unwrap();
        let source = nested.join("main.kira");
        std::fs::write(&source, "function f() { return }").unwrap();
        assert_eq!(
            manifest_for(&source).unwrap().expect("a manifest").kind(),
            PackageKind::Library
        );
    }

    #[test]
    fn a_malformed_manifest_is_an_error_not_a_silent_default() {
        // Falling back to "app" here would build the wrong kind of artifact and
        // say nothing about why.
        let dir = TempDir::new("malformed");
        std::fs::write(
            dir.path().join(DECLARATION_MANIFEST_FILE_NAME),
            "Package broken {\n let kind = .Plugin\n}",
        )
        .unwrap();
        let source = dir.path().join("main.kira");
        std::fs::write(&source, "function f() { return }").unwrap();
        assert!(matches!(
            manifest_for(&source),
            Err(DiscoveryError::Malformed { .. })
        ));
    }

    #[test]
    fn an_app_manifest_reports_the_app_kind() {
        let dir = TempDir::new("app");
        std::fs::write(
            dir.path().join(DECLARATION_MANIFEST_FILE_NAME),
            "Package demo {\n let kind = .App\n}",
        )
        .unwrap();
        let source = dir.path().join("main.kira");
        std::fs::write(&source, "@Main function main() { return }").unwrap();
        assert_eq!(
            manifest_for(&source).unwrap().expect("a manifest").kind(),
            PackageKind::App
        );
    }

    #[test]
    fn declaration_manifest_wins_precedence() {
        assert_eq!(DECLARATION_MANIFEST_FILE_NAME, MANIFEST_FILE_NAMES[0]);
        assert!(is_declaration_manifest("some/dir/package.kira"));
        assert!(!is_declaration_manifest("some/dir/kira.toml"));
    }
}
