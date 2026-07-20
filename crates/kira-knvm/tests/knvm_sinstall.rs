//! `sinstall`: knvm and the launcher, built from the checkout, put on PATH.
//!
//! Like the `binstall` tests, these run the real cargo build in the real
//! checkout, and land the result in throwaway directories: a temp kira home
//! for the binaries and a temp shell home for the startup files. The
//! developer's real home is never read or written.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};

use kira_knvm::{BinstallError, sinstall};

/// A temp directory that removes itself, so a failing assert cannot leak a tree.
struct TempTree {
    path: PathBuf,
}

impl TempTree {
    fn create(label: &str) -> Self {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        let pid = std::process::id();
        let path = std::env::temp_dir().join(format!("knvm_sinstall_{label}_{pid}_{unique}"));
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

/// The whole self-install: both tools land, both run, the shell is configured.
#[test]
fn sinstall_lands_both_tools_and_configures_the_shell() {
    let kira_home = TempTree::create("home");
    let shell_home = TempTree::create("shell");
    std::fs::write(shell_home.path().join(".zshenv"), "# existing contents\n")
        .expect("seed a startup file");

    let installed = sinstall(
        kira_home.path(),
        shell_home.path(),
        Some("/bin/zsh"),
        &repository_root(),
    )
    .expect("sinstall from this checkout");

    assert_eq!(installed.bin_dir, kira_home.path().join("bin"));
    for tool in ["knvm", "kira", "kira-language-server"] {
        let binary = installed.bin_dir.join(tool);
        assert!(binary.is_file(), "`{tool}` must be installed");
        // The installed knvm answers `help`; the installed launcher — under
        // either of its names — with no toolchain selected under this home,
        // must refuse with its own exit code 2 rather than something
        // unrelated. All three prove the binaries run.
        let (arguments, expected): (&[&str], i32) = match tool {
            "knvm" => (&["help"], 0),
            _ => (&[], 2),
        };
        let output = Command::new(&binary)
            .args(arguments)
            .env("KIRA_HOME", kira_home.path())
            .output()
            .expect("run the installed tool");
        assert_eq!(
            output.status.code(),
            Some(expected),
            "`{tool}` exit: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let env_script = kira_home.path().join("env");
    assert_eq!(installed.env_script, env_script);
    let env_contents = std::fs::read_to_string(&env_script).expect("read the env script");
    assert!(
        env_contents.contains(&format!("{}:$PATH", installed.bin_dir.display())),
        "the env script must prepend the bin dir: {env_contents}"
    );

    let startup =
        std::fs::read_to_string(shell_home.path().join(".zshenv")).expect("read the startup file");
    assert!(
        startup.contains(&format!(". \"{}\"", env_script.display())),
        "the startup file must source the env script: {startup}"
    );
    assert!(
        startup.starts_with("# existing contents"),
        "existing startup contents must survive"
    );
}

/// A second run changes nothing: no duplicate PATH lines, tools still there.
#[test]
fn a_second_sinstall_does_not_stack_path_lines() {
    let kira_home = TempTree::create("home");
    let shell_home = TempTree::create("shell");
    let checkout = repository_root();

    let first = sinstall(
        kira_home.path(),
        shell_home.path(),
        Some("/bin/bash"),
        &checkout,
    )
    .expect("first sinstall");
    assert!(first.startup_file_updated);

    let second = sinstall(
        kira_home.path(),
        shell_home.path(),
        Some("/bin/bash"),
        &checkout,
    )
    .expect("second sinstall");
    assert!(!second.startup_file_updated);
    assert_eq!(second.startup_file, shell_home.path().join(".bashrc"));

    let startup = std::fs::read_to_string(&second.startup_file).expect("read the startup file");
    let occurrences = startup.matches("/env\"").count();
    assert_eq!(
        occurrences, 1,
        "the source line must appear once: {startup}"
    );
}

/// The bug this file exists to keep dead: a bare macOS home and a zsh user.
/// zsh never reads `.profile`, so the line must land in `.zshenv`, created.
#[test]
fn a_zsh_user_with_a_bare_home_gets_a_zshenv_not_a_profile() {
    let kira_home = TempTree::create("home");
    let shell_home = TempTree::create("shell");

    let installed = sinstall(
        kira_home.path(),
        shell_home.path(),
        Some("/bin/zsh"),
        &repository_root(),
    )
    .expect("sinstall into a bare zsh home");

    assert_eq!(installed.startup_file, shell_home.path().join(".zshenv"));
    assert!(installed.startup_file.is_file(), "created, not skipped");
    assert!(
        !shell_home.path().join(".profile").exists(),
        "nothing may land in a file zsh never reads"
    );
}

/// A shell nobody recognizes falls back to the POSIX `.profile`.
#[test]
fn an_unknown_shell_falls_back_to_profile() {
    let kira_home = TempTree::create("home");
    let shell_home = TempTree::create("shell");

    let installed = sinstall(
        kira_home.path(),
        shell_home.path(),
        None,
        &repository_root(),
    )
    .expect("sinstall with no shell");

    assert_eq!(installed.startup_file, shell_home.path().join(".profile"));
    assert!(installed.startup_file.is_file());
}

/// Refusal outside a checkout: there is nothing to build the tools from.
#[test]
fn sinstall_refuses_a_directory_that_is_not_a_checkout() {
    let kira_home = TempTree::create("home");
    let shell_home = TempTree::create("shell");
    let elsewhere = TempTree::create("elsewhere");

    let error = sinstall(kira_home.path(), shell_home.path(), None, elsewhere.path())
        .expect_err("no checkout, no tools");
    assert!(
        matches!(error, BinstallError::NotACheckout { .. }),
        "got: {error}"
    );
    assert!(
        !kira_home.path().join("bin").exists(),
        "a refused sinstall must not create the bin directory"
    );
}
