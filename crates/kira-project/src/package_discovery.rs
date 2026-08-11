//! Manifest discovery: file names and target resolution from paths.
//!
//! Declaration and legacy TOML manifests share one discovery path, with the
//! declaration form taking precedence when a package carries both.

use kira_manifest::{DeclarationError, LegacyManifestError, PackageKind, ProjectManifest};

use crate::project::{Project, ResolvedTarget, TargetKind};

/// The declaration manifest. It takes precedence over `kira.toml` when both
/// are present in a package directory (it is first in
/// [`MANIFEST_FILE_NAMES`]).
pub const DECLARATION_MANIFEST_FILE_NAME: &str = "package.kira";
pub const PREFERRED_MANIFEST_FILE_NAME: &str = "kira.toml";
pub const LEGACY_MANIFEST_FILE_NAME: &str = "project.toml";
pub const REPO_MANIFEST_FILE_NAME: &str = "Kira.toml";
pub const MANIFEST_FILE_NAME: &str = PREFERRED_MANIFEST_FILE_NAME;
pub const ENTRYPOINT_REL_PATH: &str = "app/main.kira";

/// The directory a `*_types.kira` foreign-binding vocabulary file must live in.
pub const BIND_TYPES_DIR_NAME: &str = "bind-types";

/// The filename suffix marking a foreign-binding type-vocabulary source.
pub const BIND_TYPES_FILE_SUFFIX: &str = "_types.kira";

/// Whether `path` is a `*_types.kira` source placed outside a `bind-types/`
/// directory.
///
/// A `*_types.kira` file is the convention for hand-authored foreign-binding
/// type vocabulary — the C primitive typedefs a generated binding assumes — and
/// must sit directly inside a `bind-types/` directory, separate from a package's
/// own `types/` domain types and from generated `bindings/`. A path whose file
/// name does not end in `_types.kira` is never misplaced; one that does is
/// misplaced unless its immediate parent directory is named `bind-types`.
#[must_use]
pub fn is_misplaced_bind_types_file(path: &std::path::Path) -> bool {
    let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    if !file_name.ends_with(BIND_TYPES_FILE_SUFFIX) {
        return false;
    }
    path.parent()
        .and_then(std::path::Path::file_name)
        .and_then(|name| name.to_str())
        != Some(BIND_TYPES_DIR_NAME)
}

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
    /// A legacy TOML manifest was found but could not be decoded.
    #[error("cannot read the legacy package manifest `{path}`: {message}")]
    LegacyMalformed {
        /// The manifest that could not be parsed.
        path: String,
        /// Why parsing failed.
        message: String,
    },
    /// A directory target did not contain a recognized manifest.
    #[error(
        "`{path}` is not a Kira package directory: expected one of `package.kira`, `kira.toml`, `project.toml`, or `Kira.toml`"
    )]
    NotPackageDirectory {
        /// The directory supplied by the user.
        path: String,
    },
    /// An application package did not contain its conventional entrypoint.
    #[error("application package `{package}` has no entrypoint at `{path}`")]
    MissingEntrypoint {
        /// The package declared by the manifest.
        package: String,
        /// The conventional entrypoint path.
        path: String,
    },
    /// A library package had no source file that could seed compilation.
    #[error("library package `{package}` has no `.kira` sources under `{path}`")]
    NoLibrarySources {
        /// The package declared by the manifest.
        package: String,
        /// The package's application source directory.
        path: String,
    },
}

/// The manifest governing `source`, found by walking up from its directory.
///
/// `Ok(None)` means no recognized manifest sits above the file. That is not an
/// error: a bare `.kira` file handed to `kira` is a program in its own right,
/// and the caller supplies the default. A manifest that *is* present and
/// unreadable is an error, because silently falling back to the default would
/// build the wrong kind of thing.
///
/// `package.kira` wins when it sits beside a legacy TOML file. The TOML names
/// are parsed through [`kira_manifest::load_legacy_manifest`] rather than
/// ignored, so an older package cannot silently be treated as a standalone
/// source file.
pub fn manifest_for(source: &std::path::Path) -> Result<Option<Manifest>, DiscoveryError> {
    let start = if source.is_dir() {
        Some(source)
    } else {
        source.parent()
    };
    let mut dir = start;
    while let Some(current) = dir {
        for file_name in MANIFEST_FILE_NAMES {
            let candidate = current.join(file_name);
            if !candidate.is_file() {
                continue;
            }
            let path = candidate.display().to_string();
            let text = std::fs::read_to_string(&candidate).map_err(|error| {
                DiscoveryError::Unreadable {
                    path: path.clone(),
                    message: error.to_string(),
                }
            })?;
            let manifest = if file_name == DECLARATION_MANIFEST_FILE_NAME {
                kira_manifest::load_declaration(&text).map_err(|source| {
                    DiscoveryError::Malformed {
                        path: path.clone(),
                        source,
                    }
                })?
            } else {
                kira_manifest::load_legacy_manifest(&text).map_err(|source| {
                    DiscoveryError::LegacyMalformed {
                        path: path.clone(),
                        message: legacy_error_message(source),
                    }
                })?
            };
            return Ok(Some(Manifest { path, manifest }));
        }
        dir = current.parent();
    }
    Ok(None)
}

/// Resolves a source-file or package-directory argument to the file that seeds compilation.
///
/// Non-directory paths are returned unchanged so standalone `.kira` files keep
/// their historical behavior, including error reporting for a missing path. A
/// package directory is governed by its own manifest: applications use
/// [`ENTRYPOINT_REL_PATH`], while libraries choose the entry from their complete
/// [`LibrarySources`] set so the frontend can aggregate the rest of the package
/// source tree.
pub fn resolve_target(path: &std::path::Path) -> Result<ResolvedTarget, DiscoveryError> {
    if !path.is_dir() {
        return Ok(ResolvedTarget {
            root_path: None,
            manifest_path: None,
            source_path: Some(path.display().to_string()),
            source_root: path.parent().map(|parent| parent.display().to_string()),
            project_name: None,
            project: None,
            package_kind: None,
            target_kind: TargetKind::SourceFile,
        });
    }

    let Some(found) = manifest_for(path)? else {
        return Err(DiscoveryError::NotPackageDirectory {
            path: path.display().to_string(),
        });
    };
    let (source_path, target_kind) = match found.kind() {
        PackageKind::App => {
            let entrypoint = path.join(ENTRYPOINT_REL_PATH);
            if !entrypoint.is_file() {
                return Err(DiscoveryError::MissingEntrypoint {
                    package: found.manifest.name.clone(),
                    path: entrypoint.display().to_string(),
                });
            }
            (entrypoint, TargetKind::Executable)
        }
        PackageKind::Library => {
            let sources = library_sources(&found)?;
            (sources.entry().path().to_path_buf(), TargetKind::Library)
        }
    };
    let source_root = match found.kind() {
        PackageKind::App => path.join("app"),
        PackageKind::Library => library_source_root(&found),
    };
    let manifest_path = found.path.clone();
    let project_name = found.manifest.name.clone();
    let package_kind = found.manifest.kind;
    let project = Project {
        manifest: found.manifest,
    };

    Ok(ResolvedTarget {
        root_path: Some(path.display().to_string()),
        manifest_path: Some(manifest_path),
        source_path: Some(source_path.display().to_string()),
        source_root: Some(source_root.display().to_string()),
        project_name: Some(project_name),
        project: Some(project),
        package_kind: Some(package_kind),
        target_kind,
    })
}

fn legacy_error_message(error: LegacyManifestError) -> String {
    error.to_string()
}

/// One source owned by a library package, with its import-visible module name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LibrarySource {
    path: std::path::PathBuf,
    module: String,
}

impl LibrarySource {
    /// The source path exactly as discovered below the package source root.
    pub fn path(&self) -> &std::path::Path {
        &self.path
    }

    /// The dotted module name corresponding to the source's path below the
    /// package source root.
    pub fn module(&self) -> &str {
        &self.module
    }
}

/// Every source owned by a library, with the deterministic compilation entry separated out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LibrarySources {
    entry: LibrarySource,
    remaining: Vec<LibrarySource>,
}

impl LibrarySources {
    /// The conventional module-root source, or the first sorted source when it is absent.
    pub fn entry(&self) -> &LibrarySource {
        &self.entry
    }

    /// Every library source exactly once, with the compilation entry first.
    pub fn iter(&self) -> impl Iterator<Item = &LibrarySource> {
        std::iter::once(&self.entry).chain(self.remaining.iter())
    }
}

/// Discovers every `.kira` source below a library package's source root.
///
/// Paths retain the spelling derived from the manifest path so diagnostics keep
/// pointing at the same source names the package resolver found. The returned
/// order is deterministic, with the conventional module-root file first when it
/// exists and all remaining files sorted by path. New packages conventionally
/// put sources under `app/`; a package without that directory may keep its
/// sources beside `package.kira`, which is supported for the original library
/// layout used by the examples.
pub fn library_sources(manifest: &Manifest) -> Result<LibrarySources, DiscoveryError> {
    let source_root = library_source_root(manifest);
    let mut paths = Vec::new();
    collect_kira_sources(&source_root, &mut paths)?;
    paths.sort();
    if paths.is_empty() {
        return Err(DiscoveryError::NoLibrarySources {
            package: manifest.manifest.name.clone(),
            path: source_root.display().to_string(),
        });
    }

    let module_root = manifest
        .manifest
        .module_root
        .as_deref()
        .unwrap_or(&manifest.manifest.name);
    let conventional = source_root.join(format!("{module_root}.kira"));
    let entry_index = paths
        .iter()
        .position(|path| path == &conventional)
        .unwrap_or(0);
    let entry_path = paths.remove(entry_index);
    let entry = library_source(&source_root, entry_path);
    let remaining = paths
        .into_iter()
        .map(|path| library_source(&source_root, path))
        .collect();
    Ok(LibrarySources { entry, remaining })
}

/// Discovers all library sources when `entry` is inside the package's source tree.
///
/// `Ok(None)` preserves explicit compilation of a package-adjacent source file:
/// only the conventional package source tree has aggregate library semantics.
pub fn library_sources_for_entry(
    manifest: &Manifest,
    entry: &std::path::Path,
) -> Result<Option<LibrarySources>, DiscoveryError> {
    let source_root = library_source_root(manifest);
    if !path_identity(entry).starts_with(path_identity(&source_root)) {
        return Ok(None);
    }
    library_sources(manifest).map(Some)
}

/// Returns the source root beside a manifest.
///
/// `app/` remains the preferred layout. Falling back to the package directory
/// keeps older and small library packages source-compatible without making
/// applications give up their required `app/main.kira` entrypoint.
fn library_source_root(manifest: &Manifest) -> std::path::PathBuf {
    let manifest_path = std::path::Path::new(&manifest.path);
    let package_root = match manifest_path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => std::path::Path::new("."),
    };
    let app = package_root.join("app");
    if app.is_dir() {
        app
    } else {
        package_root.to_path_buf()
    }
}

/// Produces a stable identity for package-boundary comparisons.
fn path_identity(path: &std::path::Path) -> std::path::PathBuf {
    std::fs::canonicalize(path)
        .or_else(|_| std::path::absolute(path))
        .unwrap_or_else(|_| path.to_path_buf())
}

/// Gives one path from the package-owned walk its dotted module name.
fn library_source(source_root: &std::path::Path, path: std::path::PathBuf) -> LibrarySource {
    let relative = path.strip_prefix(source_root).unwrap_or(&path);
    let mut module_path = relative.to_path_buf();
    module_path.set_extension("");
    let module = module_path
        .iter()
        .map(|segment| segment.to_string_lossy())
        .collect::<Vec<_>>()
        .join(".");
    LibrarySource { path, module }
}

/// Collects every Kira source below a library's source root.
fn collect_kira_sources(
    directory: &std::path::Path,
    sources: &mut Vec<std::path::PathBuf>,
) -> Result<(), DiscoveryError> {
    let entries = std::fs::read_dir(directory).map_err(|error| DiscoveryError::Unreadable {
        path: directory.display().to_string(),
        message: error.to_string(),
    })?;
    for entry in entries {
        let entry = entry.map_err(|error| DiscoveryError::Unreadable {
            path: directory.display().to_string(),
            message: error.to_string(),
        })?;
        let path = entry.path();
        if path.is_dir() {
            // A dot-directory is never package source. `.kira-build/` holds
            // generated code and stale copies of the package's own files, so
            // walking into it would make every name in the package a duplicate
            // of itself.
            if path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with('.'))
            {
                continue;
            }
            collect_kira_sources(&path, sources)?;
        } else if path
            .extension()
            .is_some_and(|extension| extension == "kira")
        {
            // In the root-layout fallback the declaration manifest itself is
            // beside the source files. It is data for discovery, not a module
            // to feed to the frontend. `linter.kira` is likewise attached by
            // program assembly as a control module with its own fixed name.
            let control_file = path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| matches!(name, DECLARATION_MANIFEST_FILE_NAME | "linter.kira"));
            if !control_file {
                sources.push(path);
            }
        }
    }
    Ok(())
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
    fn a_source_file_target_is_returned_without_rewriting_its_path() {
        let path = std::path::Path::new("relative/missing.kira");
        let target = resolve_target(path).expect("source paths are unchanged");
        assert_eq!(target.source_path.as_deref(), Some("relative/missing.kira"));
        assert_eq!(target.target_kind, TargetKind::SourceFile);
    }

    #[test]
    fn an_app_directory_resolves_to_its_conventional_entrypoint() {
        let dir = TempDir::new("app-target");
        std::fs::write(
            dir.path().join(DECLARATION_MANIFEST_FILE_NAME),
            "Package demo {\n let kind = .App\n}",
        )
        .unwrap();
        let entrypoint = dir.path().join(ENTRYPOINT_REL_PATH);
        std::fs::create_dir_all(entrypoint.parent().expect("entrypoint parent")).unwrap();
        std::fs::write(&entrypoint, "@Main function main() { return }").unwrap();

        let target = resolve_target(dir.path()).expect("resolve app directory");
        assert_eq!(target.source_path.as_deref(), entrypoint.to_str());
        assert_eq!(target.target_kind, TargetKind::Executable);
        assert_eq!(target.package_kind, Some(PackageKind::App));
    }

    #[test]
    fn a_library_directory_uses_a_deterministic_app_source() {
        let dir = TempDir::new("library-target");
        std::fs::write(
            dir.path().join(DECLARATION_MANIFEST_FILE_NAME),
            "Package Core {\n let kind = .Library\n let moduleRoot = \"Core\"\n}",
        )
        .unwrap();
        // Joined component by component: `join` keeps an embedded `/` verbatim
        // on Windows, so the path would carry a separator discovery never
        // produces and the comparison below would fail there only.
        let source = dir.path().join("app").join("Core.kira");
        std::fs::create_dir_all(source.parent().expect("source parent")).unwrap();
        std::fs::write(&source, "function value() -> Int { return 1 }").unwrap();
        let nested = dir.path().join("app/Detail/Value.kira");
        std::fs::create_dir_all(nested.parent().expect("nested source parent")).unwrap();
        std::fs::write(&nested, "function detail() -> Int { return 2 }").unwrap();
        let other = dir.path().join("app/Another.kira");
        std::fs::write(&other, "function another() -> Int { return 3 }").unwrap();

        let target = resolve_target(dir.path()).expect("resolve library directory");
        assert_eq!(target.source_path.as_deref(), source.to_str());
        assert_eq!(target.target_kind, TargetKind::Library);
        assert!(!target.can_run());

        let manifest = manifest_for(dir.path())
            .expect("discover manifest")
            .expect("library manifest");
        let sources = library_sources(&manifest).expect("discover all library sources");
        let discovered = sources
            .iter()
            .map(|source| (source.module(), source.path()))
            .collect::<Vec<_>>();
        assert_eq!(
            discovered,
            vec![
                ("Core", source.as_path()),
                ("Another", other.as_path()),
                ("Detail.Value", nested.as_path()),
            ]
        );
        let adjacent = dir.path().join("Standalone.kira");
        std::fs::write(&adjacent, "function standalone() { return }").unwrap();
        assert_eq!(library_sources_for_entry(&manifest, &adjacent), Ok(None));
    }

    #[test]
    fn a_library_without_app_uses_sources_beside_the_manifest() {
        let dir = TempDir::new("library-root");
        std::fs::write(
            dir.path().join(DECLARATION_MANIFEST_FILE_NAME),
            "Package greetings {\n let kind = .Library\n}",
        )
        .unwrap();
        let source = dir.path().join("greetings.kira");
        std::fs::write(&source, "function hello() -> String { return \"hi\" }").unwrap();
        let nested = dir.path().join("detail").join("format.kira");
        std::fs::create_dir_all(nested.parent().expect("nested source parent")).unwrap();
        std::fs::write(&nested, "function format() -> String { return \"ok\" }").unwrap();

        let target = resolve_target(dir.path()).expect("resolve root-layout library");
        assert_eq!(target.source_path.as_deref(), source.to_str());
        assert_eq!(target.source_root.as_deref(), dir.path().to_str());
        assert_eq!(target.target_kind, TargetKind::Library);

        let manifest = manifest_for(dir.path())
            .expect("discover manifest")
            .expect("library manifest");
        let sources = library_sources(&manifest).expect("discover root-layout sources");
        let discovered = sources
            .iter()
            .map(|source| (source.module(), source.path()))
            .collect::<Vec<_>>();
        assert_eq!(
            discovered,
            vec![
                ("greetings", source.as_path()),
                ("detail.format", nested.as_path())
            ]
        );
        assert!(
            library_sources_for_entry(&manifest, &source)
                .expect("entry is in the package source root")
                .is_some()
        );
    }

    #[test]
    fn declaration_manifest_wins_precedence() {
        assert_eq!(DECLARATION_MANIFEST_FILE_NAME, MANIFEST_FILE_NAMES[0]);
        assert!(is_declaration_manifest("some/dir/package.kira"));
        assert!(!is_declaration_manifest("some/dir/kira.toml"));
    }

    #[test]
    fn a_legacy_toml_manifest_is_discovered_and_resolves_an_app() {
        let dir = TempDir::new("legacy");
        std::fs::write(
            dir.path().join(PREFERRED_MANIFEST_FILE_NAME),
            "[package]\nname = \"legacy\"\nversion = \"0.1.0\"\nkind = \"app\"\n",
        )
        .unwrap();
        let entrypoint = dir.path().join(ENTRYPOINT_REL_PATH);
        std::fs::create_dir_all(entrypoint.parent().expect("entrypoint parent")).unwrap();
        std::fs::write(&entrypoint, "@Main function main() { return }").unwrap();

        let found = manifest_for(dir.path()).unwrap().expect("legacy manifest");
        assert_eq!(
            found.path,
            dir.path()
                .join(PREFERRED_MANIFEST_FILE_NAME)
                .display()
                .to_string()
        );
        let target = resolve_target(dir.path()).expect("legacy target");
        assert_eq!(target.package_kind, Some(PackageKind::App));
        assert_eq!(target.source_path.as_deref(), entrypoint.to_str());
    }

    #[test]
    fn declaration_manifest_wins_over_a_legacy_file_on_disk() {
        let dir = TempDir::new("precedence");
        std::fs::write(
            dir.path().join(DECLARATION_MANIFEST_FILE_NAME),
            "Package current { let kind = .Library }",
        )
        .unwrap();
        std::fs::write(
            dir.path().join(PREFERRED_MANIFEST_FILE_NAME),
            "this is not TOML =",
        )
        .unwrap();
        let found = manifest_for(dir.path()).unwrap().expect("manifest");
        assert_eq!(found.manifest.name, "current");
        assert!(found.path.ends_with(DECLARATION_MANIFEST_FILE_NAME));
    }

    #[test]
    fn bind_types_file_in_bind_types_dir_is_well_placed() {
        assert!(!is_misplaced_bind_types_file(std::path::Path::new(
            "pkg/app/bind-types/vulkan_types.kira"
        )));
    }

    #[test]
    fn bind_types_file_outside_bind_types_dir_is_misplaced() {
        assert!(is_misplaced_bind_types_file(std::path::Path::new(
            "pkg/app/types/vulkan_types.kira"
        )));
        assert!(is_misplaced_bind_types_file(std::path::Path::new(
            "pkg/app/bindings/vulkan_types.kira"
        )));
    }

    #[test]
    fn a_non_types_suffix_file_is_never_misplaced() {
        // Only `*_types.kira` is governed; `types.kira` (no underscore prefix)
        // and ordinary sources are exempt wherever they sit.
        assert!(!is_misplaced_bind_types_file(std::path::Path::new(
            "pkg/app/types/types.kira"
        )));
        assert!(!is_misplaced_bind_types_file(std::path::Path::new(
            "pkg/app/Core/Widget.kira"
        )));
    }
}
