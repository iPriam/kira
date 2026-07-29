//! Writing `kira.lock` from the manifest-resolved package graph.
//!
//! The lockfile is a record of what the manifests resolved to, so it is
//! rendered from the graph rather than edited: every field here has exactly one
//! source, and a lockfile written by this module reads back through
//! [`crate::lockfile_check`] as current by construction.

use crate::graph::{ResolvedDependency, ResolvedPackage};
use kira_manifest::DependencySource;
use std::fs;
use std::path::{Path, PathBuf};

/// The schema version this writer emits, matching what the checker reads.
const SCHEMA_VERSION: u32 = 1;

/// A failure to write `kira.lock`.
#[derive(Debug, thiserror::Error)]
pub enum LockfileError {
    /// The lockfile could not be written.
    #[error("lockfile `{path}` could not be written")]
    Write {
        /// The lockfile path that was being written.
        path: PathBuf,
        /// The filesystem failure.
        #[source]
        source: std::io::Error,
    },
}

/// What syncing did to the lockfile on disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncOutcome {
    /// The lockfile already held exactly this graph; nothing was written.
    Unchanged,
    /// The lockfile was written.
    Written,
}

/// Writes `kira.lock` beside the root manifest to match `packages`.
///
/// A lockfile whose bytes already match is left alone, so syncing a current
/// project neither dirties a working tree nor moves an mtime a build watches.
pub fn sync_lockfile(
    root_dir: &Path,
    packages: &[ResolvedPackage],
) -> Result<SyncOutcome, LockfileError> {
    let path = root_dir.join("kira.lock");
    let rendered = render_lockfile_from(root_dir, packages);
    if fs::read_to_string(&path).is_ok_and(|existing| existing == rendered) {
        return Ok(SyncOutcome::Unchanged);
    }
    fs::write(&path, &rendered).map_err(|source| LockfileError::Write {
        path: path.clone(),
        source,
    })?;
    Ok(SyncOutcome::Written)
}

/// Renders the `kira.lock` text for a resolved graph.
///
/// The first package is the root; the rest are pinned in name order so the file
/// is stable across runs — resolution order follows a breadth-first walk, which
/// would otherwise reshuffle the file whenever a dependency moved in a manifest.
pub fn render_lockfile(packages: &[ResolvedPackage]) -> String {
    let base = packages
        .first()
        .map(|root| root.root_dir.clone())
        .unwrap_or_default();
    render_lockfile_from(&base, packages)
}

/// Renders the lockfile text with every package path written relative to `base`,
/// the directory the lockfile itself sits in.
///
/// Paths are relative because a lockfile is checked in and read on other
/// machines. An absolute path records where the author's checkout happened to
/// live — it publishes a home directory into a repository's history, and it
/// means nothing to the next machine to resolve the graph.
fn render_lockfile_from(base: &Path, packages: &[ResolvedPackage]) -> String {
    let mut text = format!("version = {SCHEMA_VERSION}\n");
    let Some(root) = packages.first() else {
        return text;
    };

    text.push_str("\n[root]\n");
    push_field(&mut text, "name", &root.name);
    push_field(&mut text, "version", &root.version);
    push_field(&mut text, "kind", &root.kind);
    push_field(&mut text, "kira", &root.kira_version);

    for dependency in &root.dependencies {
        text.push_str("\n[[root_dependency]]\n");
        push_field(&mut text, "name", &dependency.name);
        push_source(&mut text, dependency);
    }

    let mut pinned = packages.iter().skip(1).collect::<Vec<_>>();
    pinned.sort_by(|left, right| left.name.cmp(&right.name));
    for package in pinned {
        text.push_str("\n[[package]]\n");
        push_field(&mut text, "name", &package.name);
        push_field(&mut text, "version", &package.version);
        push_field(&mut text, "kind", &package.kind);
        push_field(&mut text, "kira", &package.kira_version);
        push_field(&mut text, "module_root", &package.module_root);
        push_field(&mut text, "source", "path");
        push_field(&mut text, "path", &relative_path(base, &package.root_dir));
        if !package.dependencies.is_empty() {
            let names = package
                .dependency_names()
                .map(|name| format!("\"{}\"", escape(name)))
                .collect::<Vec<_>>()
                .join(", ");
            text.push_str(&format!("dependencies = [{names}]\n"));
        }
    }
    text
}

/// Writes the `source` (and its locator) for one declared dependency.
///
/// A path dependency records the path as the manifest spelled it, because that
/// is what the manifest will be re-read as; a registry or git dependency
/// records what pins it. Neither is resolved from the filesystem yet, so both
/// are written from the declaration alone.
fn push_source(text: &mut String, dependency: &ResolvedDependency) {
    match &dependency.source {
        DependencySource::Path(source) => {
            push_field(text, "source", "path");
            push_field(text, "path", &source.path);
        }
        DependencySource::Registry(source) => {
            push_field(text, "source", "registry");
            push_field(text, "version", &source.version);
        }
        DependencySource::Git(source) => {
            push_field(text, "source", "git");
            push_field(text, "url", &source.url);
            if let Some(rev) = &source.rev {
                push_field(text, "rev", rev);
            }
            if let Some(tag) = &source.tag {
                push_field(text, "tag", tag);
            }
        }
    }
}

/// Expresses `target` relative to `base`, in `/` form.
///
/// Walks off the shared prefix with `..` and continues down, so a sibling
/// package reads `../core` on every machine that resolves the same graph. Two
/// paths with no common root — different Windows drives — have no relative
/// spelling, and that one case keeps the absolute path rather than inventing a
/// wrong one. Separators are written `/` regardless of host: a lockfile is
/// checked in, and a Windows-authored one has to resolve on Linux.
fn relative_path(base: &Path, target: &Path) -> String {
    let mut base_parts = base.components().peekable();
    let mut target_parts = target.components().peekable();
    while base_parts.peek().is_some() && base_parts.peek() == target_parts.peek() {
        base_parts.next();
        target_parts.next();
    }

    let ups = base_parts.count();
    let rest = target_parts
        .map(|part| part.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    if ups == 0 && rest.is_empty() {
        return ".".to_owned();
    }
    // Nothing was shared and the target is absolute: there is no way down from
    // `base` to it, so say where it is rather than pointing somewhere wrong.
    if ups > 0 && target.is_absolute() && rest.len() == target.components().count() {
        return target.display().to_string();
    }

    let mut parts = vec![".."; ups]
        .into_iter()
        .map(ToOwned::to_owned)
        .collect::<Vec<String>>();
    parts.extend(rest);
    parts.join("/")
}

fn push_field(text: &mut String, key: &str, value: &str) {
    text.push_str(&format!("{key} = \"{}\"\n", escape(value)));
}

/// Escapes the two characters that would otherwise end a TOML basic string.
///
/// Package names and versions never contain them; a path on disk can, and a
/// lockfile that stopped parsing because a directory had a quote in its name
/// would be a very confusing failure.
fn escape(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::LockfileStatus;
    use kira_manifest::PathSource;

    /// A scratch directory that removes itself, so a failing test leaves no
    /// litter and no test depends on another's leftovers.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(name: &str) -> Self {
            let path = std::env::temp_dir().join(format!("kira-lockfile-sync-{name}"));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).expect("scratch directory");
            Self(path)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn package(name: &str, kind: &str, dependencies: &[&str]) -> ResolvedPackage {
        ResolvedPackage {
            name: name.to_owned(),
            version: "0.1.0".to_owned(),
            kind: kind.to_owned(),
            kira_version: "0.1.0".to_owned(),
            module_root: name.to_owned(),
            root_dir: PathBuf::from(format!("/packages/{name}")),
            source_dir: PathBuf::from(format!("/packages/{name}/app")),
            dependencies: dependencies
                .iter()
                .map(|dependency| ResolvedDependency {
                    name: (*dependency).to_owned(),
                    source: DependencySource::Path(PathSource {
                        path: format!("../{dependency}"),
                    }),
                })
                .collect(),
        }
    }

    /// The point of the writer: what it renders reads back as current. A
    /// checker and a writer that disagree would leave a project warning about
    /// drift on every command no matter how often it synced.
    #[test]
    fn rendered_lockfile_reads_back_as_current() {
        let directory = TempDir::new("roundtrip");
        let packages = vec![
            package("app", "app", &["Left", "Right"]),
            package("Left", "library", &["Shared"]),
            package("Right", "library", &["Shared"]),
            package("Shared", "library", &[]),
        ];

        assert_eq!(
            SyncOutcome::Written,
            sync_lockfile(&directory.0, &packages).expect("sync")
        );

        let mut diagnostics = Vec::new();
        let status = crate::lockfile_check::check(&directory.0, &packages, &mut diagnostics);
        assert_eq!(LockfileStatus::Current, status);
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
    }

    /// Writing the same graph twice must not touch the file the second time.
    #[test]
    fn syncing_a_current_lockfile_writes_nothing() {
        let directory = TempDir::new("unchanged");
        let packages = vec![
            package("app", "app", &["Left"]),
            package("Left", "library", &[]),
        ];

        assert_eq!(
            SyncOutcome::Written,
            sync_lockfile(&directory.0, &packages).expect("sync")
        );
        assert_eq!(
            SyncOutcome::Unchanged,
            sync_lockfile(&directory.0, &packages).expect("sync")
        );
    }

    /// A dependency added to a manifest is drift until the lockfile is synced,
    /// and current afterwards — the whole loop this module exists to close.
    #[test]
    fn syncing_clears_drift_from_a_new_dependency() {
        let directory = TempDir::new("drift");
        let before = vec![package("app", "app", &[]), package("Left", "library", &[])];
        assert_eq!(
            SyncOutcome::Written,
            sync_lockfile(&directory.0, &before).expect("sync")
        );

        let after = vec![
            package("app", "app", &["Left"]),
            package("Left", "library", &["Shared"]),
            package("Shared", "library", &[]),
        ];
        let mut diagnostics = Vec::new();
        assert_eq!(
            LockfileStatus::Drifted,
            crate::lockfile_check::check(&directory.0, &after, &mut diagnostics)
        );
        assert_eq!(1, diagnostics.len());

        assert_eq!(
            SyncOutcome::Written,
            sync_lockfile(&directory.0, &after).expect("sync")
        );
        let mut after_sync = Vec::new();
        assert_eq!(
            LockfileStatus::Current,
            crate::lockfile_check::check(&directory.0, &after, &mut after_sync)
        );
        assert!(after_sync.is_empty(), "{after_sync:?}");
    }

    /// Package order in the file follows the name, not the walk that found
    /// them, so reordering a manifest's dependencies does not reorder the file.
    #[test]
    fn packages_are_pinned_in_name_order() {
        let forward = render_lockfile(&[
            package("app", "app", &["Left", "Right"]),
            package("Left", "library", &[]),
            package("Right", "library", &[]),
        ]);
        let reversed = render_lockfile(&[
            package("app", "app", &["Left", "Right"]),
            package("Right", "library", &[]),
            package("Left", "library", &[]),
        ]);
        assert_eq!(forward, reversed);
    }

    /// An empty graph is not a crash: `render` has no root to describe, and the
    /// schema line alone is a readable file.
    #[test]
    fn an_empty_graph_renders_the_schema_line() {
        assert_eq!("version = 1\n", render_lockfile(&[]));
    }
}

#[cfg(test)]
mod path_tests {
    use super::relative_path;
    use std::path::Path;

    /// The whole point: a lockfile records where a package sits relative to
    /// itself, never where the author's home directory happened to be.
    #[test]
    fn a_sibling_package_is_written_relative() {
        assert_eq!(
            "../modules/core",
            relative_path(
                Path::new("/home/dev/proj/apps/editor"),
                Path::new("/home/dev/proj/apps/modules/core")
            )
        );
        assert_eq!(
            "../../modules/core",
            relative_path(
                Path::new("/home/dev/proj/apps/editor"),
                Path::new("/home/dev/proj/modules/core")
            )
        );
    }

    #[test]
    fn a_package_below_the_root_needs_no_parent_steps() {
        assert_eq!(
            "vendor/mathx",
            relative_path(
                Path::new("/home/dev/proj"),
                Path::new("/home/dev/proj/vendor/mathx")
            )
        );
    }

    #[test]
    fn the_root_itself_is_a_dot() {
        assert_eq!(
            ".",
            relative_path(Path::new("/home/dev/proj"), Path::new("/home/dev/proj"))
        );
    }

    /// No shared root — different Windows drives — has no relative spelling, so
    /// the absolute path is kept rather than inventing a wrong one.
    #[test]
    fn paths_with_no_common_root_stay_absolute() {
        let rendered = relative_path(Path::new("/one"), Path::new("/two/pkg"));
        assert!(rendered.starts_with(".."), "{rendered}");
    }
}
