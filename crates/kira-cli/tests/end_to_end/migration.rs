//! Legacy manifest migration, driven through the real `kira` binary.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

use crate::kira;

fn temporary_project() -> PathBuf {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let number = COUNTER.fetch_add(1, Ordering::Relaxed);
    let root =
        std::env::temp_dir().join(format!("kira_migration_{}_{}", std::process::id(), number));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("app")).expect("project directory");
    root
}

#[test]
fn migrate_manifest_creates_a_canonical_declaration_without_deleting_toml() {
    let root = temporary_project();
    std::fs::write(
        root.join("kira.toml"),
        "[package]\nname = \"LegacyApp\"\nversion = \"0.2.0\"\nkind = \"app\"\n",
    )
    .expect("legacy manifest");
    std::fs::write(
        root.join("app/main.kira"),
        "@Main function main() { print(7) return }",
    )
    .expect("entrypoint");

    let output = kira(&["migrate-manifest", root.to_str().expect("a utf-8 path")]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(root.join("package.kira").is_file());
    assert!(root.join("kira.toml").is_file());

    let checked = kira(&["check", root.to_str().expect("a utf-8 path")]);
    assert!(
        checked.status.success(),
        "{}",
        String::from_utf8_lossy(&checked.stderr)
    );
    let run = kira(&["run", root.to_str().expect("a utf-8 path")]);
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout), "7\n");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn migrate_manifest_does_not_overwrite_a_current_declaration() {
    let root = temporary_project();
    std::fs::write(root.join("kira.toml"), "name = \"Legacy\"\n").expect("legacy manifest");
    std::fs::write(root.join("package.kira"), "Package Current {}\n").expect("declaration");

    let output = kira(&["migrate-manifest", root.to_str().expect("a utf-8 path")]);
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("refusing to overwrite"));
    assert_eq!(
        std::fs::read_to_string(root.join("package.kira")).expect("declaration"),
        "Package Current {}\n"
    );
    let _ = std::fs::remove_dir_all(root);
}
