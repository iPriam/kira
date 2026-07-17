//! Watching a program's inputs for a change worth rebuilding.
//!
//! A live session polls rather than subscribing to the OS. Polling is
//! unglamorous and portable, and a live session's watch set is a program's
//! sources — small enough that a stat per file every few hundred milliseconds is
//! not worth a platform-specific API and its edge cases.
//!
//! The interesting part is not the polling; it is **what is not watched**. A
//! watcher that notices its own build output rebuilds forever: the build writes,
//! the watcher sees a change, it rebuilds, the build writes. So build outputs are
//! excluded, and so is editor noise — an editor that writes `app.kira~` and
//! `.app.kira.swp` on the way to saving would otherwise trigger three rebuilds
//! per save, two of them of an unchanged program.
//!
//! Both exclusion lists are matched on every host rather than on the one that
//! produces them: a session is run on a machine whose editor and toolchain are
//! not knowable from here.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// Directory names whose contents are never watched.
///
/// Every one of these is somewhere a build writes. Watching any of them makes a
/// session rebuild in a loop, which is the failure this list exists to prevent.
const IGNORED_DIRECTORIES: [&str; 5] = [".kira-build", "exports", "zig-out", "generated", "target"];

/// File suffixes that are never watched.
///
/// Editors write these on the way to saving a file. They are noise: the real
/// save arrives as a change to the real file a moment later.
const IGNORED_SUFFIXES: [&str; 4] = ["~", ".swp", ".swx", ".tmp"];

/// What a file looked like last time it was checked.
///
/// Modification time and size together: a change that moves neither is a change
/// no poller can see, and the alternative — hashing every input every tick —
/// costs more than it is worth for a watch set that is mostly source files. A
/// filesystem with coarse timestamps could hide an edit that also preserved the
/// size; on the platforms this runs on, timestamps are nanosecond-resolution and
/// that case does not arise.
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

/// The inputs a live session rebuilds from.
///
/// Roots are files and directories; a directory root is walked, and everything
/// under it that is not excluded is watched. A session names its roots once and
/// the watcher re-walks them each poll, so a file created after the session
/// started is picked up rather than needing a restart.
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
        self.roots.push(path.into());
        self
    }

    /// The roots, in the order they were added.
    pub fn roots(&self) -> &[PathBuf] {
        &self.roots
    }

    /// Every watchable file under the roots, walked fresh.
    ///
    /// Sorted, because the order two directory reads come back in is the
    /// filesystem's business, and a session that reported its changes in a
    /// different order each run would be a session nobody could test.
    pub fn files(&self) -> Vec<PathBuf> {
        let mut found = Vec::new();
        for root in &self.roots {
            collect(root, &mut found);
        }
        found.sort();
        found.dedup();
        found
    }
}

/// Walks `path`, pushing every watchable file into `found`.
///
/// Errors are silence rather than failure: a directory that cannot be read this
/// tick is a directory with no watchable files this tick, and a live session must
/// not die because an editor replaced a directory while it was being walked.
fn collect(path: &Path, found: &mut Vec<PathBuf>) {
    // Depth is what stops a symlink cycle. `is_dir` follows links, so a single
    // `a -> .` inside a watched tree makes the walk descend forever — and two of
    // them make it branch, so the work doubles per level until the path outgrows
    // the platform's limit. That is not a hang anyone diagnoses quickly: the
    // session simply never starts. A tree deeper than this is not a source tree.
    const MAX_DEPTH: usize = 32;

    fn walk(path: &Path, depth: usize, found: &mut Vec<PathBuf>) {
        if path.is_file() {
            if is_watchable_file(path) {
                found.push(path.to_owned());
            }
            return;
        }
        if depth >= MAX_DEPTH || !path.is_dir() || !is_watchable_directory(path) {
            return;
        }
        let Ok(entries) = std::fs::read_dir(path) else {
            return;
        };
        for entry in entries.flatten() {
            walk(&entry.path(), depth + 1, found);
        }
    }

    walk(path, 0, found);
}

/// Whether a directory's contents are watched.
///
/// Excludes build outputs and every dot-directory. Dot-directories go wholesale
/// rather than by name: `.git`, `.svn`, and an editor's private directory are all
/// churn a program's behavior does not depend on, and naming them one at a time
/// is a list that is always one entry out of date.
pub fn is_watchable_directory(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return true;
    };
    if name.starts_with('.') {
        return false;
    }
    !IGNORED_DIRECTORIES.contains(&name)
}

/// Whether a file is watched.
///
/// Excludes dotfiles and editor scratch. A dotfile is not a program input; an
/// editor's scratch file is the same save arriving twice.
pub fn is_watchable_file(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    if name.starts_with('.') {
        return false;
    }
    !IGNORED_SUFFIXES.iter().any(|suffix| name.ends_with(suffix))
}

/// Watches a [`WatchSet`], reporting what changed since the last look.
///
/// The watcher is a snapshot and a comparison, with no thread and no channel: a
/// session asks when it is ready to rebuild, which means a burst of saves during
/// a build collapses into one rebuild rather than queueing three.
#[derive(Debug, Clone)]
pub struct SourceWatcher {
    set: WatchSet,
    seen: BTreeMap<PathBuf, Stamp>,
}

impl SourceWatcher {
    /// Starts watching `set`, taking the current state as the baseline.
    ///
    /// The baseline is taken now, so the files that already exist are not
    /// reported as added the first time [`SourceWatcher::poll`] is called — a
    /// session must not rebuild immediately because its program exists.
    pub fn new(set: WatchSet) -> SourceWatcher {
        let seen = snapshot(&set);
        SourceWatcher { set, seen }
    }

    /// The set being watched.
    pub fn set(&self) -> &WatchSet {
        &self.set
    }

    /// Everything that changed since the last poll, and adopts the new state.
    ///
    /// Empty when nothing changed, which is the answer almost every time.
    pub fn poll(&mut self) -> Vec<Change> {
        let now = snapshot(&self.set);
        let mut changes = Vec::new();

        for (path, stamp) in &now {
            match self.seen.get(path) {
                None => changes.push(Change {
                    path: path.clone(),
                    kind: ChangeKind::Added,
                }),
                Some(before) if before != stamp => changes.push(Change {
                    path: path.clone(),
                    kind: ChangeKind::Modified,
                }),
                Some(_) => {}
            }
        }
        for path in self.seen.keys() {
            if !now.contains_key(path) {
                changes.push(Change {
                    path: path.clone(),
                    kind: ChangeKind::Removed,
                });
            }
        }

        self.seen = now;
        changes
    }
}

/// Stamps every watchable file under `set`.
fn snapshot(set: &WatchSet) -> BTreeMap<PathBuf, Stamp> {
    set.files()
        .into_iter()
        .filter_map(|path| Stamp::of(&path).map(|stamp| (path, stamp)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// A scratch directory that removes itself.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> TempDir {
            let path =
                std::env::temp_dir().join(format!("kira-watch-{}-{tag}", std::process::id()));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).expect("scratch dir");
            TempDir(path)
        }

        fn write(&self, name: &str, contents: &str) -> PathBuf {
            let path = self.0.join(name);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).expect("parent");
            }
            fs::write(&path, contents).expect("write");
            path
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// Makes a later write land on a different modification time.
    ///
    /// A same-size write in the same timestamp tick is invisible to a stat-based
    /// watcher. The tests are about the watcher's logic, not the filesystem's
    /// clock, so they set the stamp rather than racing it.
    fn touch_distinctly(path: &Path, contents: &str) {
        fs::write(path, contents).expect("write");
        let later = SystemTime::now() + std::time::Duration::from_secs(2);
        let _ = filetime_set(path, later);
    }

    /// Sets a file's modification time.
    ///
    /// Hand-rolled rather than a dependency: the workspace treats dependencies as
    /// frozen, and a test helper is not a reason to add one.
    fn filetime_set(path: &Path, time: SystemTime) -> std::io::Result<()> {
        let file = fs::OpenOptions::new().write(true).open(path)?;
        file.set_modified(time)
    }

    #[test]
    fn a_new_file_is_added() {
        let dir = TempDir::new("added");
        let mut watcher = SourceWatcher::new(WatchSet::new().root(&dir.0));
        assert!(watcher.poll().is_empty(), "nothing changed yet");

        let path = dir.write("app.kira", "@Main function main() { return }");
        let changes = watcher.poll();
        assert_eq!(
            changes,
            vec![Change {
                path,
                kind: ChangeKind::Added
            }]
        );
    }

    #[test]
    fn an_edited_file_is_modified() {
        let dir = TempDir::new("modified");
        let path = dir.write("app.kira", "before");
        let mut watcher = SourceWatcher::new(WatchSet::new().root(&dir.0));

        touch_distinctly(&path, "after!");
        let changes = watcher.poll();
        assert_eq!(
            changes,
            vec![Change {
                path,
                kind: ChangeKind::Modified
            }]
        );
    }

    #[test]
    fn a_deleted_file_is_removed() {
        let dir = TempDir::new("removed");
        let path = dir.write("app.kira", "x");
        let mut watcher = SourceWatcher::new(WatchSet::new().root(&dir.0));

        fs::remove_file(&path).expect("remove");
        assert_eq!(
            watcher.poll(),
            vec![Change {
                path,
                kind: ChangeKind::Removed
            }]
        );
    }

    /// The baseline is taken at construction, so a session does not rebuild the
    /// instant it starts just because its program is on disk.
    #[test]
    fn an_untouched_watch_set_reports_nothing() {
        let dir = TempDir::new("quiet");
        dir.write("app.kira", "x");
        let mut watcher = SourceWatcher::new(WatchSet::new().root(&dir.0));
        assert!(watcher.poll().is_empty());
        assert!(watcher.poll().is_empty(), "and it stays quiet");
    }

    /// A change is reported once. A watcher that re-reported it would rebuild
    /// forever off one save.
    #[test]
    fn a_change_is_reported_once() {
        let dir = TempDir::new("once");
        let path = dir.write("app.kira", "before");
        let mut watcher = SourceWatcher::new(WatchSet::new().root(&dir.0));

        touch_distinctly(&path, "after!");
        assert_eq!(watcher.poll().len(), 1);
        assert!(watcher.poll().is_empty(), "the same change came back");
    }

    /// The rule that keeps a session from rebuilding itself to death: a build
    /// writing into its own output directory is not a source change.
    #[test]
    fn build_output_never_triggers_a_rebuild() {
        let dir = TempDir::new("output");
        dir.write("app.kira", "x");
        let mut watcher = SourceWatcher::new(WatchSet::new().root(&dir.0));

        for output in [
            ".kira-build/app.o",
            "exports/app.zip",
            "zig-out/bin/app",
            "generated/bindings.kira",
            "target/debug/app",
        ] {
            dir.write(output, "build output");
        }

        assert!(
            watcher.poll().is_empty(),
            "a build's own output triggered a rebuild"
        );
    }

    /// An editor writing scratch files on the way to a save must produce one
    /// rebuild, not three.
    #[test]
    fn editor_noise_never_triggers_a_rebuild() {
        let dir = TempDir::new("noise");
        dir.write("app.kira", "x");
        let mut watcher = SourceWatcher::new(WatchSet::new().root(&dir.0));

        for noise in [
            "app.kira~",
            ".app.kira.swp",
            "app.kira.swx",
            "app.kira.tmp",
            ".DS_Store",
            ".hidden",
        ] {
            dir.write(noise, "noise");
        }

        assert!(
            watcher.poll().is_empty(),
            "editor noise triggered a rebuild"
        );
    }

    /// A dot-directory is excluded wholesale: `.git` churns constantly and none
    /// of it is a program input.
    #[test]
    fn dot_directories_are_not_watched() {
        let dir = TempDir::new("dotdir");
        dir.write("app.kira", "x");
        let mut watcher = SourceWatcher::new(WatchSet::new().root(&dir.0));

        dir.write(".git/HEAD", "ref: refs/heads/main");
        dir.write(".kira-build/nested/deep/thing.o", "output");

        assert!(watcher.poll().is_empty());
    }

    /// A real source edit still gets through, with all that filtering in place.
    /// Without this, a watcher that ignored everything would pass every test
    /// above.
    #[test]
    fn a_real_source_edit_still_gets_through() {
        let dir = TempDir::new("real");
        let source = dir.write("app.kira", "before");
        dir.write("shader.ksl", "shader");
        let mut watcher = SourceWatcher::new(WatchSet::new().root(&dir.0));

        // Noise and output alongside the real edit: the real one must survive.
        dir.write("app.kira~", "noise");
        dir.write(".kira-build/app.o", "output");
        touch_distinctly(&source, "after!");

        let changes = watcher.poll();
        assert_eq!(
            changes,
            vec![Change {
                path: source,
                kind: ChangeKind::Modified
            }],
            "the real edit must be the only change reported"
        );
    }

    /// Shaders and assets are program inputs too, not just `.kira` files.
    #[test]
    fn shaders_and_assets_are_watched() {
        let dir = TempDir::new("inputs");
        let shader = dir.write("shaders/main.ksl", "before");
        let asset = dir.write("assets/logo.png", "before");
        let mut watcher = SourceWatcher::new(WatchSet::new().root(&dir.0));

        touch_distinctly(&shader, "after!");
        touch_distinctly(&asset, "after!");

        let mut changed: Vec<PathBuf> = watcher.poll().into_iter().map(|c| c.path).collect();
        changed.sort();
        let mut expected = vec![shader, asset];
        expected.sort();
        assert_eq!(changed, expected);
    }

    /// A single file is a legitimate watch set: it is what a session watching one
    /// program has.
    #[test]
    fn a_single_file_root_is_watched() {
        let dir = TempDir::new("single");
        let source = dir.write("app.kira", "before");
        dir.write("other.kira", "untouched");
        let mut watcher = SourceWatcher::new(WatchSet::new().root(&source));

        touch_distinctly(&source, "after!");
        assert_eq!(watcher.poll().len(), 1);

        // A file outside the set is not the session's business.
        touch_distinctly(&dir.0.join("other.kira"), "changed");
        assert!(watcher.poll().is_empty());
    }

    #[test]
    fn a_missing_root_is_not_an_error() {
        let mut watcher =
            SourceWatcher::new(WatchSet::new().root("/nonexistent/path/to/nowhere.kira"));
        assert!(watcher.poll().is_empty());
    }

    #[test]
    fn files_are_reported_in_a_stable_order() {
        let dir = TempDir::new("order");
        for name in ["c.kira", "a.kira", "b.kira"] {
            dir.write(name, "x");
        }
        let set = WatchSet::new().root(&dir.0);
        let mut sorted = set.files();
        sorted.sort();
        assert_eq!(set.files(), sorted);
    }

    #[test]
    fn change_kinds_have_labels() {
        assert_eq!(ChangeKind::Added.label(), "added");
        assert_eq!(ChangeKind::Modified.label(), "modified");
        assert_eq!(ChangeKind::Removed.label(), "removed");
    }
}
