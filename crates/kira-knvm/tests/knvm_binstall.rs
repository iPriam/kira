//! `binstall`: this checkout, built and installed as a dev toolchain.
//!
//! These run the real `cargo build -p kira-cli` in the real checkout — that is
//! the command under test, and a stub build would prove a different program.
//! The build is incremental, so after the first workspace build it costs
//! seconds, not a rebuild. The install still lands in a throwaway toolchains
//! root; the developer's `~/.kira` is never touched.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};

use kira_knvm::{BinstallError, Channel, binstall, read_current};
use kira_toolchain::{executable_name, static_archive_name};

/// A temp directory that removes itself, so a failing assert cannot leak a tree.
struct TempTree {
    path: PathBuf,
}

impl TempTree {
    fn create(label: &str) -> Self {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        let pid = std::process::id();
        let path = std::env::temp_dir().join(format!("knvm_binstall_{label}_{pid}_{unique}"));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("create temp tree");
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempTree {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// The repository root, two levels above this crate.
fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("crates/kira-knvm has a repository root above it")
        .to_path_buf()
}

/// The whole developer route: build, install, select, and compile a program
/// with the result.
#[test]
fn binstall_installs_this_checkout_as_the_selected_dev_toolchain() {
    let root = TempTree::create("root");

    let installed = binstall(root.path(), &repository_root()).expect("binstall from this checkout");

    assert_eq!(installed.channel, Channel::Dev);
    assert!(
        installed
            .root
            .join("bin")
            .join(executable_name("kira"))
            .is_file(),
        "the dev toolchain must hold the built compiler"
    );
    for archive in [
        static_archive_name("kira_native_bridge"),
        static_archive_name("kira_compiler_bridge"),
        "libkira_native_bridge-wasm32-emscripten.a".to_owned(),
    ] {
        assert!(
            installed.root.join("bin").join(&archive).is_file(),
            "the dev toolchain must hold `{archive}`"
        );
    }
    assert!(
        installed
            .root
            .join("foundation")
            .join("package.kira")
            .is_file(),
        "the dev toolchain must ship the checkout's Foundation"
    );

    let current = read_current(root.path())
        .expect("read the selection")
        .expect("binstall selects what it installs");
    assert_eq!(current.channel, Channel::Dev);
    assert_eq!(current.version, installed.version);

    // The built compiler, from inside the installed tree, on a program with
    // nothing beside it: the tree has to be self-sufficient.
    let workspace = TempTree::create("program");
    let program = workspace.path().join("main.kira");
    std::fs::write(
        &program,
        "import Foundation\n@Main function main() { printLine(\"dev toolchain\") return }",
    )
    .expect("write the program");
    let output = Command::new(installed.root.join("bin").join(executable_name("kira")))
        .arg("run")
        .arg(&program)
        .current_dir(workspace.path())
        .output()
        .expect("run the dev compiler");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout), "dev toolchain\n");
}

/// A second binstall replaces the tree. "Already installed" would mean
/// "silently stale" for a dev build, which is the fake success this test pins
/// out of existence.
#[test]
fn a_second_binstall_replaces_the_previous_build() {
    let root = TempTree::create("root");
    let checkout = repository_root();

    let first = binstall(root.path(), &checkout).expect("first binstall");
    assert!(!first.already_installed);

    // A witness the replacement must not carry over.
    let witness = first.root.join("stale-witness");
    std::fs::write(&witness, "from the first build").expect("write the witness");

    let second = binstall(root.path(), &checkout).expect("second binstall");
    assert!(second.already_installed, "the rebuild must say it replaced");
    assert_eq!(second.root, first.root);
    assert!(
        !witness.exists(),
        "a rebuild must land a fresh tree, not touch up the old one"
    );
    assert!(
        second
            .root
            .join("bin")
            .join(executable_name("kira"))
            .is_file(),
        "the replacement must be a whole toolchain"
    );
}

/// Refusal outside a checkout, so `binstall` in the wrong directory cannot
/// build whatever workspace it happens to be standing in.
#[test]
fn binstall_refuses_a_directory_that_is_not_a_checkout() {
    let root = TempTree::create("root");
    let elsewhere = TempTree::create("elsewhere");
    // A Rust workspace without a Foundation is not a Kira checkout.
    std::fs::write(elsewhere.path().join("Cargo.toml"), "[workspace]\n").expect("write manifest");

    let error = binstall(root.path(), elsewhere.path())
        .expect_err("a directory without the checkout markers cannot be binstalled");
    assert!(
        matches!(error, BinstallError::NotACheckout { .. }),
        "got: {error}"
    );
    assert!(
        read_current(root.path())
            .expect("read the selection")
            .is_none(),
        "a refused binstall must not select anything"
    );
}
