//! Dependency mutation commands, driven through the real `kira` binary.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

use crate::kira;

fn temporary_project() -> PathBuf {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let number = COUNTER.fetch_add(1, Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!(
        "kira_dependencies_{}_{}",
        std::process::id(),
        number
    ));
    let _ = std::fs::remove_dir_all(&root);
    root
}

#[test]
fn add_and_remove_round_trip_a_path_dependency() {
    let root = temporary_project();
    let root_text = root.to_str().expect("a utf-8 path");
    let created = kira(&["new", root_text]);
    assert!(
        created.status.success(),
        "{}",
        String::from_utf8_lossy(&created.stderr)
    );

    let added = kira(&["add", "Core", "--path", "../core", root_text]);
    assert!(
        added.status.success(),
        "{}",
        String::from_utf8_lossy(&added.stderr)
    );
    let manifest = std::fs::read_to_string(root.join("package.kira")).expect("manifest");
    assert!(manifest.contains("Dependency { name: \"Core\", path: \"../core\" }"));

    let duplicate = kira(&["add", "Core", "--path", "../other", root_text]);
    assert_eq!(duplicate.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&duplicate.stderr).contains("already declared"));

    let removed = kira(&["remove", "Core", root_text]);
    assert!(
        removed.status.success(),
        "{}",
        String::from_utf8_lossy(&removed.stderr)
    );
    let manifest = std::fs::read_to_string(root.join("package.kira")).expect("manifest");
    assert!(!manifest.contains("Core"));
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn add_accepts_registry_and_git_sources() {
    let root = temporary_project();
    let root_text = root.to_str().expect("a utf-8 path");
    let created = kira(&["new", root_text]);
    assert!(
        created.status.success(),
        "{}",
        String::from_utf8_lossy(&created.stderr)
    );

    for command in [
        vec!["add", "Registry", "--version", "1.2.3", root_text],
        vec![
            "add",
            "Git",
            "--git",
            "https://example.test/repo.git",
            "--rev",
            "abc",
            root_text,
        ],
    ] {
        let output = kira(&command);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let manifest = std::fs::read_to_string(root.join("package.kira")).expect("manifest");
    assert!(manifest.contains("version: \"1.2.3\""));
    assert!(manifest.contains("url: \"https://example.test/repo.git\""));
    assert!(manifest.contains("rev: \"abc\""));
    let _ = std::fs::remove_dir_all(root);
}
