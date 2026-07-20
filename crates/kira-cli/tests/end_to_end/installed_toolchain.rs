//! Foundation resolved out of a toolchain `knvm` installed, not out of this
//! checkout.
//!
//! Every other Foundation case runs `kirac` from `target/`, where the checkout
//! rule answers first: the binary walks up, finds the workspace `Cargo.toml`
//! beside `foundation/`, and never consults `current.toml`. That is the
//! developer path, and it proves nothing about the shipped one.
//!
//! Here the compiler runs under a throwaway `KIRA_HOME` with no checkout above
//! it, against a tree the real `kira_knvm::install` laid down — so a layout
//! change that broke discovery fails here rather than passing against a tree
//! the test shaped to agree with it.
//!
//! The two cases are the two rules a released compiler can resolve by, and they
//! are not interchangeable. A compiler *inside* a toolchain answers from the
//! `foundation/` beside its own `bin/` and never reads `current.toml`; only a
//! compiler outside one falls through to the selection. Testing the first and
//! calling it the second is the mistake this file exists to avoid.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};

use kira_knvm::{Channel, DirectoryReleaseSource, VersionSpec, archive_file_name, install};

/// The version the fixture release publishes.
///
/// This crate's own version, because the archive holds this crate's own
/// compiler: an invented release number would put a version on disk that names
/// a toolchain nobody built.
const FIXTURE_VERSION: &str = env!("CARGO_PKG_VERSION");

/// A temp directory that removes itself, so a failing assert cannot leak a tree.
struct TempTree {
    path: PathBuf,
}

impl TempTree {
    fn create(label: &str) -> Self {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        let pid = std::process::id();
        let path = std::env::temp_dir().join(format!("kirac_installed_{label}_{pid}_{unique}"));
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

/// The toolchains root inside a `KIRA_HOME`.
///
/// `knvm`'s library takes this root explicitly while discovery derives it from
/// `KIRA_HOME`, so a test that hands the two different directories proves
/// nothing — it is the one seam where they have to agree.
fn toolchains_root(kira_home: &Path) -> PathBuf {
    kira_home.join("toolchains")
}

/// Copies a directory tree, preserving executable bits on unix.
fn copy_tree(source: &Path, destination: &Path) {
    std::fs::create_dir_all(destination).expect("create destination");
    for entry in std::fs::read_dir(source).expect("read source") {
        let entry = entry.expect("read entry");
        let target = destination.join(entry.file_name());
        if entry.file_type().expect("file type").is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).expect("copy file");
        }
    }
}

/// The repository root, two levels above this crate.
fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("crates/kira-cli has a repository root above it")
        .to_path_buf()
}

/// Publishes a release archive holding this checkout's real compiler and
/// Foundation, shaped the way a released toolchain is.
///
/// The compiler is the binary under test, so what this proves is discovery and
/// layout rather than some fixture stand-in: the same `kirac` that passes the
/// checkout-rule tests has to find Foundation with the checkout taken away.
fn publish_fixture_release(feed: &Path, staging: &Path) {
    let payload = staging.join(format!("kira-{FIXTURE_VERSION}"));
    let bin = payload.join("bin");
    std::fs::create_dir_all(&bin).expect("create bin");
    std::fs::copy(env!("CARGO_BIN_EXE_kirac"), bin.join("kirac")).expect("stage the compiler");
    copy_tree(
        &repository_root().join("foundation"),
        &payload.join("foundation"),
    );

    let host_key = kira_knvm::current_host_key().expect("a supported host");
    let channel_dir = feed.join(Channel::Release.dir_name()).join(FIXTURE_VERSION);
    std::fs::create_dir_all(&channel_dir).expect("create the feed directory");

    let archive = channel_dir.join(archive_file_name(FIXTURE_VERSION, host_key));
    let packed = Command::new("tar")
        .arg("-czf")
        .arg(&archive)
        .arg("-C")
        .arg(staging)
        .arg(format!("kira-{FIXTURE_VERSION}"))
        .status()
        .expect("run tar");
    assert!(packed.success(), "packing the fixture release must succeed");
}

/// The shipped rule against a real install: `knvm` lays the tree down, and the
/// compiler inside it finds the `foundation/` beside its own `bin/`.
#[test]
fn a_knvm_installed_compiler_resolves_the_foundation_beside_it() {
    let home = TempTree::create("home");
    let feed = TempTree::create("feed");
    let staging = TempTree::create("staging");
    publish_fixture_release(feed.path(), staging.path());

    let source = DirectoryReleaseSource::new(feed.path()).expect("a supported host");
    let installed = install(
        &toolchains_root(home.path()),
        &source,
        &VersionSpec::Exact(FIXTURE_VERSION.to_string()),
        Channel::Release,
    )
    .expect("install the fixture toolchain");

    // The program lives beside nothing: no package manifest, no `Foundation.kira`,
    // and no checkout anywhere above it, so nothing local can satisfy the import.
    let workspace = TempTree::create("program");
    let program = workspace.path().join("main.kira");
    std::fs::write(
        &program,
        "import Foundation\n\
         @Main function main() { printLine(\"hello from the installed Foundation\") return }",
    )
    .expect("write the program");

    let compiler = installed.root.join("bin").join("kirac");
    let output = Command::new(&compiler)
        .arg("run")
        .arg(&program)
        .env("KIRA_HOME", home.path())
        .current_dir(workspace.path())
        .output()
        .expect("run the installed compiler");

    assert!(
        output.status.success(),
        "the installed compiler must resolve its own Foundation: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "hello from the installed Foundation\n"
    );
}

/// The managed rule, which is the one the shipped rule hides above.
///
/// A compiler *inside* a toolchain tree never needs `current.toml`: its own
/// `foundation/` sits beside its `bin/` and the shipped rule answers first. The
/// rule only decides anything for a compiler that is not in such a tree, so this
/// runs one from a bare directory — no `foundation/` beside it, no checkout
/// above it — where `current.toml` is the only thing left that can answer.
#[test]
fn a_compiler_outside_any_toolchain_resolves_through_the_selection() {
    let home = TempTree::create("home");
    let feed = TempTree::create("feed");
    let staging = TempTree::create("staging");
    publish_fixture_release(feed.path(), staging.path());

    let source = DirectoryReleaseSource::new(feed.path()).expect("a supported host");
    install(
        &toolchains_root(home.path()),
        &source,
        &VersionSpec::Exact(FIXTURE_VERSION.to_string()),
        Channel::Release,
    )
    .expect("install the fixture toolchain");

    // Deliberately not in the installed tree and not in the checkout: the two
    // executable-relative rules must both miss.
    let elsewhere = TempTree::create("elsewhere");
    let compiler = elsewhere.path().join("kirac");
    std::fs::copy(env!("CARGO_BIN_EXE_kirac"), &compiler).expect("stage a loose compiler");

    let program = elsewhere.path().join("main.kira");
    std::fs::write(
        &program,
        "import Foundation\n\
         @Main function main() { printLine(\"resolved through current.toml\") return }",
    )
    .expect("write the program");

    let selected = Command::new(&compiler)
        .arg("run")
        .arg(&program)
        .env("KIRA_HOME", home.path())
        .current_dir(elsewhere.path())
        .output()
        .expect("run the loose compiler");
    assert!(
        selected.status.success(),
        "`current.toml` must point a loose compiler at the installed Foundation: {}",
        String::from_utf8_lossy(&selected.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&selected.stdout),
        "resolved through current.toml\n"
    );

    // Dropping the selection is what proves the success above came from it.
    std::fs::remove_file(toolchains_root(home.path()).join("current.toml"))
        .expect("drop the selection");
    let unselected = Command::new(&compiler)
        .arg("check")
        .arg(&program)
        .env("KIRA_HOME", home.path())
        .current_dir(elsewhere.path())
        .output()
        .expect("run the loose compiler");
    assert!(
        !unselected.status.success(),
        "with nothing selected a loose compiler has no Foundation to import"
    );
}
