//! `kira new`, driven through the real binary and then checked by Kira.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

use crate::kira;

fn temporary_project(label: &str) -> PathBuf {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let number = COUNTER.fetch_add(1, Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!(
        "kira_new_{label}_{}_{}",
        std::process::id(),
        number
    ));
    let _ = std::fs::remove_dir_all(&root);
    root
}

#[test]
fn new_app_creates_a_package_that_checks_and_runs() {
    let root = temporary_project("app");
    let root_text = root.to_str().expect("a utf-8 path");
    let created = kira(&["new", root_text]);
    assert!(
        created.status.success(),
        "{}",
        String::from_utf8_lossy(&created.stderr)
    );

    let checked = kira(&["check", root_text]);
    assert!(
        checked.status.success(),
        "{}",
        String::from_utf8_lossy(&checked.stderr)
    );
    let run = kira(&["run", root_text]);
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    let name = root
        .file_name()
        .and_then(|name| name.to_str())
        .expect("a project name");
    assert_eq!(
        String::from_utf8_lossy(&run.stdout),
        format!("hello from {name}\n")
    );
    assert!(root.join("package.kira").is_file());
    assert!(root.join("app/main.kira").is_file());
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn new_library_creates_a_checkable_library_package() {
    let root = temporary_project("library");
    let root_text = root.to_str().expect("a utf-8 path");
    let created = kira(&["new", "--library", root_text]);
    assert!(
        created.status.success(),
        "{}",
        String::from_utf8_lossy(&created.stderr)
    );

    let checked = kira(&["check", root_text]);
    assert!(
        checked.status.success(),
        "{}",
        String::from_utf8_lossy(&checked.stderr)
    );
    let package = std::fs::read_to_string(root.join("package.kira")).expect("manifest");
    assert!(package.contains(".Library"));
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn new_refuses_to_overwrite_an_existing_directory() {
    let root = temporary_project("existing");
    std::fs::create_dir_all(&root).expect("directory");
    std::fs::write(root.join("keep.txt"), "keep").expect("file");
    let output = kira(&["new", root.to_str().expect("a utf-8 path")]);

    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("not empty"));
    assert_eq!(
        std::fs::read_to_string(root.join("keep.txt")).expect("file"),
        "keep"
    );
    let _ = std::fs::remove_dir_all(root);
}
