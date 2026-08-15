//! Watching a program's inputs for a change worth rebuilding.
//!
//! The operating system owns change notification. The watcher registers the
//! relevant directories once, drains a short burst of notifications into one
//! batch, and snapshots only after that batch says there may be a source change.
//! Build output and editor scratch still go through the same source-set filter,
//! so a build cannot wake its own rebuild loop.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf, absolute};
use std::sync::mpsc::Receiver;
use std::time::{Duration, SystemTime};

use notify::event::{CreateKind, RemoveKind};
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode};

/// Directory names whose contents are never watched.
const IGNORED_DIRECTORIES: [&str; 11] = [
    ".git",
    ".hg",
    ".svn",
    ".bzr",
    ".jj",
    ".kira-build",
    "exports",
    "generated",
    "target",
    "zig-out",
    "build",
];

/// File suffixes that are never watched.
const IGNORED_SUFFIXES: [&str; 10] = [
    "~",
    ".swp",
    ".swo",
    ".swx",
    ".tmp",
    ".bak",
    ".orig",
    ".rej",
    ".crdownload",
    ".part",
];

/// The quiet period that closes one editor save into one rebuild batch.
pub const DEBOUNCE_WINDOW: Duration = Duration::from_millis(75);

/// The maximum time one burst may remain open while an editor keeps reporting.
const MAX_DEBOUNCE: Duration = Duration::from_millis(500);

/// What a file looked like last time it was checked.
///
/// Metadata remains useful for rescan notifications and for changes that arrive
/// without a file-specific event. A file-specific write event is authoritative,
/// which is what makes a same-size edit on a coarse-mtime filesystem visible.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Stamp {
    modified: Option<SystemTime>,
    size: u64,
}

impl Stamp {
    /// Stamps `path`, or `None` if it cannot be read.
    fn of(path: &Path) -> Option<Stamp> {
        let metadata = std::fs::metadata(path).ok()?;
        Some(Stamp {
            modified: metadata.modified().ok(),
            size: metadata.len(),
        })
    }
}

/// A change the watcher saw.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    /// The file that changed.
    pub path: PathBuf,
    /// What happened to it.
    pub kind: ChangeKind,
}

/// What happened to a watched file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeKind {
    /// A file that was not there is there now.
    Added,
    /// A file's contents moved.
    Modified,
    /// A file that was there is gone.
    Removed,
}

impl ChangeKind {
    /// A label for diagnostics.
    pub fn label(self) -> &'static str {
        match self {
            Self::Added => "added",
            Self::Modified => "modified",
            Self::Removed => "removed",
        }
    }
}

/// An operating-system watcher could not start or stopped delivering events.
#[derive(Debug, thiserror::Error)]
pub enum WatchError {
    /// The platform watcher could not be initialized.
    #[error("could not initialize the live file watcher: {0}")]
    Initialize(#[source] notify::Error),
    /// A watch root could not be registered.
    #[error("could not watch live path `{path}`: {source}")]
    Register {
        /// The path registration failed for.
        path: PathBuf,
        /// The platform error.
        #[source]
        source: notify::Error,
    },
    /// A watch root could not be removed from the platform watcher.
    #[error("could not stop watching live path `{path}`: {source}")]
    Unregister {
        /// The path removal failed for.
        path: PathBuf,
        /// The platform error.
        #[source]
        source: notify::Error,
    },
    /// The platform watcher stopped sending notifications.
    #[error("the live file watcher stopped delivering notifications")]
    Disconnected,
    /// The platform reported an event error.
    #[error("the live file watcher reported an error for {paths:?}: {source}")]
    Event {
        /// Paths associated with the platform error.
        paths: Vec<PathBuf>,
        /// The platform error.
        #[source]
        source: notify::Error,
    },
}

impl WatchError {
    /// Whether the watcher can be recreated without ending the live session.
    pub fn is_transient(&self) -> bool {
        match self {
            Self::Register { source, .. }
            | Self::Unregister { source, .. }
            | Self::Event { source, .. } => is_transient_notify_error(source),
            Self::Disconnected => true,
            Self::Initialize(_) => false,
        }
    }
}

/// Whether a platform error describes a path that can disappear during a save.
fn is_transient_notify_error(error: &notify::Error) -> bool {
    match &error.kind {
        notify::ErrorKind::PathNotFound | notify::ErrorKind::WatchNotFound => true,
        notify::ErrorKind::Io(source) => source.kind() == std::io::ErrorKind::NotFound,
        notify::ErrorKind::Generic(_)
        | notify::ErrorKind::InvalidConfig(_)
        | notify::ErrorKind::MaxFilesWatch => false,
    }
}

/// The inputs a live session rebuilds from.
///
/// Roots are files and directories; a directory root is walked, and everything
/// under it that is not excluded is watched. The operating-system watcher is
/// registered against the containing directory, so a file root survives an
/// atomic replacement and a directory root sees files created after startup.
#[derive(Debug, Clone, Default)]
pub struct WatchSet {
    roots: Vec<PathBuf>,
}

impl WatchSet {
    /// An empty watch set.
    pub fn new() -> WatchSet {
        WatchSet { roots: Vec::new() }
    }

    /// Adds a root — a file to watch, or a directory to walk.
    pub fn root(mut self, path: impl Into<PathBuf>) -> WatchSet {
        let path = normalize_path(&path.into());
        if !self.roots.iter().any(|root| same_path(root, &path)) {
            self.roots.push(path);
        }
        self
    }

    /// The roots, in the order they were added.
    pub fn roots(&self) -> &[PathBuf] {
        &self.roots
    }

    /// Every watchable file under the roots, walked fresh.
    ///
    /// Sorted because directory iteration order belongs to the filesystem.
    pub fn files(&self) -> Vec<PathBuf> {
        let mut found: Vec<PathBuf> = Vec::new();
        for root in &self.roots {
            collect(root, &mut found);
        }
        found.sort_by_key(|left| path_key(left));
        found.dedup_by(|left, right| same_path(left, right));
        found
    }

    /// Directories against which the platform watcher can register.
    fn event_roots(&self) -> Vec<(PathBuf, RecursiveMode)> {
        let mut found: Vec<(PathBuf, RecursiveMode)> = Vec::new();
        for root in &self.roots {
            if root.is_dir() {
                add_event_root(&mut found, root.clone(), RecursiveMode::Recursive);
                if let Some(parent) = root.parent() {
                    add_event_root(
                        &mut found,
                        parent.to_path_buf(),
                        RecursiveMode::NonRecursive,
                    );
                }
                continue;
            }

            let Some(parent) = root.parent() else {
                continue;
            };
            if parent.is_dir() {
                add_event_root(
                    &mut found,
                    parent.to_path_buf(),
                    RecursiveMode::NonRecursive,
                );
            } else if let Some(ancestor) = existing_directory(parent) {
                // A missing root still needs a notification when its first
                // directory is created. The fallback watches the nearest
                // existing ancestor and relies on the source-set filter.
                add_event_root(&mut found, ancestor, RecursiveMode::Recursive);
            }
        }
        found
    }
}

/// Adds one existing platform root, merging duplicate registrations.
fn add_event_root(
    found: &mut Vec<(PathBuf, RecursiveMode)>,
    candidate: PathBuf,
    mode: RecursiveMode,
) {
    if !candidate.is_dir() {
        return;
    }
    if let Some((_, existing_mode)) = found
        .iter_mut()
        .find(|(existing, _)| same_path(existing, &candidate))
    {
        if mode == RecursiveMode::Recursive {
            *existing_mode = mode;
        }
    } else {
        found.push((normalize_path(&candidate), mode));
    }
}

/// Finds the nearest directory that still exists above `path`.
fn existing_directory(path: &Path) -> Option<PathBuf> {
    let mut candidate = Some(normalize_path(path));
    while let Some(path) = candidate {
        let parent = path.parent().map(Path::to_path_buf);
        if path.is_dir() && parent.as_deref().is_some_and(|parent| parent != path) {
            return Some(path);
        }
        candidate = parent.filter(|parent| parent != &path);
    }
    None
}

/// Walks `path`, pushing every watchable file into `found`.
fn collect(path: &Path, found: &mut Vec<PathBuf>) {
    const MAX_DEPTH: usize = 64;
    const MAX_SYMLINK_DEPTH: usize = 8;

    fn walk(
        path: &Path,
        depth: usize,
        symlink_depth: usize,
        directories: &mut BTreeSet<String>,
        found: &mut Vec<PathBuf>,
    ) {
        let Ok(link_metadata) = std::fs::symlink_metadata(path) else {
            return;
        };
        let is_symlink = link_metadata.file_type().is_symlink();
        if is_symlink && symlink_depth >= MAX_SYMLINK_DEPTH {
            return;
        }
        let Ok(metadata) = std::fs::metadata(path) else {
            return;
        };
        if metadata.is_file() {
            if is_watchable_file(path) {
                found.push(path.to_owned());
            }
            return;
        }
        if depth >= MAX_DEPTH || !metadata.is_dir() || !is_watchable_directory(path) {
            return;
        }
        if !directories.insert(path_key(path)) {
            return;
        }
        let Ok(entries) = std::fs::read_dir(path) else {
            return;
        };
        for entry in entries.flatten() {
            walk(
                &entry.path(),
                depth + 1,
                symlink_depth + usize::from(is_symlink),
                directories,
                found,
            );
        }
    }

    walk(path, 0, 0, &mut BTreeSet::new(), found);
}

/// Whether a directory's contents are watched.
pub fn is_watchable_directory(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return true;
    };
    !is_ignored_directory_name(name)
}

/// Whether a file is watched.
pub fn is_watchable_file(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return true;
    };
    if name.starts_with('.') {
        return false;
    }
    let name = name.to_ascii_lowercase();
    if IGNORED_SUFFIXES.iter().any(|suffix| name.ends_with(suffix)) {
        return false;
    }
    !(name.starts_with('#') && name.ends_with('#'))
}

/// Whether a path component names a directory excluded from a watch tree.
fn is_ignored_directory_name(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    name.starts_with('.') || IGNORED_DIRECTORIES.contains(&name.as_str())
}

/// Watches a [`WatchSet`], reporting what changed since the last event batch.
pub struct SourceWatcher {
    set: WatchSet,
    seen: BTreeMap<PathBuf, Stamp>,
    _watcher: RecommendedWatcher,
    events: Receiver<notify::Result<Event>>,
    registered: Vec<(PathBuf, RecursiveMode)>,
}

/// Events that can represent a file becoming, remaining, or ceasing to exist.
fn event_kind_can_change_files(kind: &EventKind) -> bool {
    matches!(
        kind,
        EventKind::Any
            | EventKind::Create(_)
            | EventKind::Modify(_)
            | EventKind::Remove(_)
            | EventKind::Other
    )
}

/// Stamps every watchable file under `set`.
fn snapshot(set: &WatchSet) -> BTreeMap<PathBuf, Stamp> {
    set.files()
        .into_iter()
        .filter_map(|path| Stamp::of(&path).map(|stamp| (path, stamp)))
        .collect()
}

/// A path spelling used only for identity comparisons.
fn path_key(path: &Path) -> String {
    let mut key = canonicalize_existing_prefix(path)
        .to_string_lossy()
        .replace('\\', "/");
    if cfg!(windows) {
        key = key.to_lowercase();
    }
    while key.len() > 1 && key.ends_with('/') {
        key.pop();
    }
    key
}

/// Resolves the existing part of a path while retaining missing trailing
/// components. This makes aliases through a symlinked source root compare as
/// one path without making a missing source impossible to watch.
fn canonicalize_existing_prefix(path: &Path) -> PathBuf {
    let normalized = normalize_path(path);
    let mut missing = Vec::new();
    let mut candidate = normalized.clone();

    loop {
        if let Ok(canonical) = std::fs::canonicalize(&candidate) {
            let mut result = canonical;
            for component in missing.iter().rev() {
                result.push(component);
            }
            return normalize_path(&result);
        }

        let Some(name) = candidate.file_name() else {
            break;
        };
        missing.push(name.to_owned());
        let Some(parent) = candidate.parent() else {
            break;
        };
        if parent == candidate {
            break;
        }
        candidate = parent.to_owned();
    }

    normalized
}

/// Makes a path absolute and lexically removes `.` and `..` components.
///
/// This deliberately does not canonicalize: a missing path must remain
/// watchable through its existing parent, and resolving symlinks here would
/// change the source tree that the watcher is meant to filter.
fn normalize_path(path: &Path) -> PathBuf {
    let absolute = absolute(path).unwrap_or_else(|_| path.to_path_buf());
    let mut normalized = PathBuf::new();
    let mut normal_components = 0usize;
    for component in absolute.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if normal_components > 0 {
                    let _ = normalized.pop();
                    normal_components -= 1;
                }
            }
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(component.as_os_str()),
            Component::Normal(part) => {
                normalized.push(part);
                normal_components += 1;
            }
        }
    }
    normalized
}

/// Whether two path spellings identify the same file on this host.
fn same_path(left: &Path, right: &Path) -> bool {
    path_key(left) == path_key(right)
}

/// Returns `path` relative to `root` using host path identity rules.
fn relative_path(path: &Path, root: &Path) -> Option<String> {
    let path = path_key(path);
    let root = path_key(root);
    if path == root {
        return Some(String::new());
    }
    let rest = path.strip_prefix(&root)?.strip_prefix('/')?;
    Some(rest.to_owned())
}

#[path = "watch_runtime.rs"]
mod runtime;

#[cfg(test)]
#[path = "watch_tests.rs"]
mod tests;
