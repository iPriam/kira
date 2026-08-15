use super::*;
use std::fs;
use std::sync::atomic::{AtomicU32, Ordering};

/// A scratch directory that removes itself.
struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> TempDir {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("kira-watch-{}-{tag}-{unique}", std::process::id()));
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

fn watcher(dir: &TempDir) -> SourceWatcher {
    SourceWatcher::new(WatchSet::new().root(&dir.0)).expect("watcher starts")
}

fn changes(watcher: &mut SourceWatcher) -> Vec<Change> {
    watcher
        .wait_for(Duration::from_secs(2))
        .expect("watcher wait")
}

fn write_same_size_same_mtime(path: &Path, contents: &str) {
    let before = fs::metadata(path)
        .expect("metadata")
        .modified()
        .expect("mtime");
    fs::write(path, contents).expect("write");
    let file = fs::OpenOptions::new()
        .write(true)
        .open(path)
        .expect("reopen");
    file.set_modified(before).expect("restore mtime");
}

#[test]
fn a_new_file_is_added() {
    let dir = TempDir::new("added");
    let mut watcher = watcher(&dir);

    let path = dir.write("app.kira", "@Main function main() { return }");
    assert_eq!(
        changes(&mut watcher),
        vec![Change {
            path,
            kind: ChangeKind::Added
        }]
    );
}

#[test]
fn an_edited_file_is_modified_even_when_mtime_and_size_do_not_move() {
    let dir = TempDir::new("mtime");
    let path = dir.write("app.kira", "before");
    let mut watcher = watcher(&dir);

    write_same_size_same_mtime(&path, "after!");
    assert_eq!(
        changes(&mut watcher),
        vec![Change {
            path,
            kind: ChangeKind::Modified
        }]
    );
}

#[test]
fn an_atomic_rename_reports_removed_and_added_files() {
    let dir = TempDir::new("rename");
    let old = dir.write("old.kira", "before");
    let new = dir.0.join("new.kira");
    let mut watcher = watcher(&dir);

    fs::rename(&old, &new).expect("rename");
    let mut observed = changes(&mut watcher);
    observed.sort_by(|left, right| left.path.cmp(&right.path));
    assert_eq!(
        observed,
        vec![
            Change {
                path: new,
                kind: ChangeKind::Added
            },
            Change {
                path: old,
                kind: ChangeKind::Removed
            }
        ]
    );
}

#[test]
fn an_atomic_replacement_of_a_file_root_reports_a_modification() {
    let dir = TempDir::new("replace-root");
    let source = dir.write("app.kira", "before");
    let replacement = dir.0.join("app.kira.tmp");
    let mut watcher = SourceWatcher::new(WatchSet::new().root(&source)).expect("watcher");

    fs::write(&replacement, "after!").expect("replacement");
    fs::rename(&replacement, &source).expect("atomic replacement");

    assert_eq!(
        changes(&mut watcher),
        vec![Change {
            path: source,
            kind: ChangeKind::Modified
        }]
    );
}

#[test]
fn a_deleted_file_is_removed() {
    let dir = TempDir::new("removed");
    let source = dir.write("app.kira", "before");
    let mut watcher = watcher(&dir);

    fs::remove_file(&source).expect("delete");

    assert_eq!(
        changes(&mut watcher),
        vec![Change {
            path: source,
            kind: ChangeKind::Removed
        }]
    );
}

#[test]
fn an_untouched_watch_set_reports_nothing_until_an_event_arrives() {
    let dir = TempDir::new("quiet");
    dir.write("app.kira", "x");
    let mut watcher = watcher(&dir);
    assert!(
        watcher
            .wait_for(Duration::from_millis(100))
            .expect("quiet watcher")
            .is_empty()
    );
}

#[test]
fn a_change_is_reported_once() {
    let dir = TempDir::new("once");
    let path = dir.write("app.kira", "before");
    let mut watcher = watcher(&dir);

    fs::write(&path, "after!").expect("write");
    assert_eq!(changes(&mut watcher).len(), 1);
    assert!(
        watcher
            .wait_for(Duration::from_millis(150))
            .expect("quiet after change")
            .is_empty()
    );
}

#[test]
fn rapid_edits_are_coalesced_into_one_change_batch() {
    let dir = TempDir::new("coalesced");
    let path = dir.write("app.kira", "before");
    let mut watcher = watcher(&dir);

    fs::write(&path, "middle").expect("first edit");
    fs::write(&path, "after!").expect("second edit");
    let changes = changes(&mut watcher);
    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0].kind, ChangeKind::Modified);
}

#[test]
fn build_output_never_triggers_a_rebuild() {
    let dir = TempDir::new("output");
    dir.write("app.kira", "x");
    let mut watcher = watcher(&dir);

    for output in [
        ".KIRA-BUILD/app.o",
        "exports/app.zip",
        "zig-out/bin/app",
        "generated/bindings.kira",
        "TARGET/debug/app",
    ] {
        dir.write(output, "build output");
    }

    assert!(
        watcher
            .wait_for(Duration::from_millis(250))
            .expect("output watcher")
            .is_empty()
    );
}

#[test]
fn editor_noise_never_triggers_a_rebuild() {
    let dir = TempDir::new("noise");
    dir.write("app.kira", "x");
    let mut watcher = watcher(&dir);

    for noise in [
        "app.kira~",
        ".app.kira.swp",
        "app.kira.SWX",
        "app.kira.TMP",
        ".DS_Store",
    ] {
        dir.write(noise, "noise");
    }

    assert!(
        watcher
            .wait_for(Duration::from_millis(250))
            .expect("noise watcher")
            .is_empty()
    );
}

#[test]
fn a_real_source_edit_survives_noise_and_output() {
    let dir = TempDir::new("real");
    let source = dir.write("app.kira", "before");
    let mut watcher = watcher(&dir);

    dir.write("app.kira~", "noise");
    dir.write(".kira-build/app.o", "output");
    fs::write(&source, "after!").expect("edit");

    assert_eq!(
        changes(&mut watcher),
        vec![Change {
            path: source,
            kind: ChangeKind::Modified
        }]
    );
}

#[test]
fn shaders_and_assets_are_watched() {
    let dir = TempDir::new("inputs");
    let shader = dir.write("shaders/main.ksl", "before");
    let asset = dir.write("assets/logo.png", "before");
    let mut watcher = watcher(&dir);

    fs::write(&shader, "after!").expect("shader edit");
    fs::write(&asset, "after!").expect("asset edit");

    let mut changed: Vec<PathBuf> = changes(&mut watcher)
        .into_iter()
        .map(|change| change.path)
        .collect();
    changed.sort();
    let mut expected = vec![shader, asset];
    expected.sort();
    assert_eq!(changed, expected);
}

#[test]
fn a_single_file_root_does_not_watch_its_siblings() {
    let dir = TempDir::new("single");
    let source = dir.write("app.kira", "before");
    let sibling = dir.write("other.kira", "untouched");
    let mut watcher = SourceWatcher::new(WatchSet::new().root(&source)).expect("watcher");

    fs::write(&sibling, "changed").expect("sibling edit");
    assert!(
        watcher
            .wait_for(Duration::from_millis(150))
            .expect("sibling watcher")
            .is_empty()
    );
    fs::write(&source, "after!").expect("source edit");
    assert_eq!(changes(&mut watcher).len(), 1);
}

#[test]
fn a_missing_root_is_not_an_error() {
    let watcher = SourceWatcher::new(WatchSet::new().root("/nonexistent/path/to/nowhere.kira"));
    assert!(watcher.is_ok());
}

#[test]
fn a_missing_nested_root_is_observed_when_it_is_created() {
    let dir = TempDir::new("missing-nested");
    let path = dir.0.join("new/deep/app.kira");
    let mut watcher = SourceWatcher::new(WatchSet::new().root(&path)).expect("watcher");

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("nested root");
    }
    fs::write(&path, "created").expect("new source");

    assert_eq!(
        changes(&mut watcher),
        vec![Change {
            path,
            kind: ChangeKind::Added
        }]
    );
}

#[test]
fn a_directory_replacement_is_seen_after_the_root_returns() {
    let dir = TempDir::new("directory-replace");
    let root = dir.0.join("package");
    let source = dir.write("package/app.kira", "before");
    let moved = dir.0.join("package-old");
    let mut watcher = SourceWatcher::new(WatchSet::new().root(&root)).expect("watcher");

    fs::rename(&root, &moved).expect("move old directory");
    fs::create_dir_all(&root).expect("replace directory");
    fs::write(&source, "after!").expect("new source");

    assert_eq!(
        changes(&mut watcher),
        vec![Change {
            path: source,
            kind: ChangeKind::Modified
        }]
    );
}

#[test]
fn transient_notify_errors_are_typed_and_classified() {
    let source = notify::Error::path_not_found();
    assert!(is_transient_notify_error(&source));
    assert!(
        WatchError::Event {
            paths: Vec::new(),
            source,
        }
        .is_transient()
    );
    assert!(!is_transient_notify_error(&notify::Error::new(
        notify::ErrorKind::MaxFilesWatch
    )));
}

#[test]
fn ignored_names_are_case_insensitive() {
    assert!(!is_watchable_directory(Path::new("TARGET")));
    assert!(!is_watchable_directory(Path::new(".KIRA-BUILD")));
    assert!(!is_watchable_file(Path::new("app.TMP")));
    assert!(!is_watchable_file(Path::new(".app.kira")));
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
