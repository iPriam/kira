//! `sinstall`: knvm and the launcher, built from the checkout, put on PATH.
//!
//! Like the `binstall` tests, these run the real cargo build in the real
//! checkout, and land the result in throwaway directories: a temp kira home
//! for the binaries and a temp shell home for the startup files. The
//! developer's real home is never read or written.
//!
//! What "on PATH" means splits by host, so the tests do too: the startup-file
//! tests are unix's, and the user-environment test is Windows'. The Windows one
//! is the only test here that touches state outside its temp trees — the user's
//! own `Path`, which is where that host keeps this — so it restores what it
//! found, on a failing assert as well as a passing one.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};

use kira_knvm::{BinstallError, PathConfigured, sinstall};

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

/// The three tools land under the names this host runs them by, and run.
#[test]
fn sinstall_lands_both_tools_and_configures_the_path() {
    let kira_home = TempTree::create("home");
    let shell_home = TempTree::create("shell");

    let installed = sinstall(
        kira_home.path(),
        shell_home.path(),
        Some("/bin/zsh"),
        &repository_root(),
    )
    .expect("sinstall from this checkout");

    assert_eq!(installed.bin_dir, kira_home.path().join("bin"));
    assert!(
        installed.path.updated(),
        "a first install must configure the PATH, not find it configured"
    );
    for tool in ["knvm", "kira", "kira-language-server"] {
        let binary = installed
            .bin_dir
            .join(kira_toolchain::executable_name(tool));
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

/// The unix half: an `env` script, and one line in the file the shell reads.
#[cfg(not(windows))]
mod startup_file {
    use super::*;

    /// The env script and the startup file it is sourced from, or the panic
    /// that says this host configured something else.
    fn startup_file_of(configured: &PathConfigured) -> (PathBuf, PathBuf, bool) {
        match configured {
            PathConfigured::StartupFile {
                env_script,
                startup_file,
                updated,
            } => (env_script.clone(), startup_file.clone(), *updated),
            other => panic!("unix configures a startup file, not {other:?}"),
        }
    }

    /// The whole unix path: env script written, startup file sourced, contents
    /// that were already there kept.
    #[test]
    fn the_startup_file_sources_the_env_script() {
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

        let (env_script, startup_file, updated) = startup_file_of(&installed.path);
        assert!(updated);
        assert_eq!(env_script, kira_home.path().join("env"));
        assert_eq!(startup_file, shell_home.path().join(".zshenv"));

        let env_contents = std::fs::read_to_string(&env_script).expect("read the env script");
        assert!(
            env_contents.contains(&format!("{}:$PATH", installed.bin_dir.display())),
            "the env script must prepend the bin dir: {env_contents}"
        );

        let startup = std::fs::read_to_string(&startup_file).expect("read the startup file");
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
        assert!(first.path.updated());

        let second = sinstall(
            kira_home.path(),
            shell_home.path(),
            Some("/bin/bash"),
            &checkout,
        )
        .expect("second sinstall");
        assert!(!second.path.updated());

        let (_, startup_file, _) = startup_file_of(&second.path);
        assert_eq!(startup_file, shell_home.path().join(".bashrc"));
        let startup = std::fs::read_to_string(&startup_file).expect("read the startup file");
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

        let (_, startup_file, _) = startup_file_of(&installed.path);
        assert_eq!(startup_file, shell_home.path().join(".zshenv"));
        assert!(startup_file.is_file(), "created, not skipped");
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

        let (_, startup_file, _) = startup_file_of(&installed.path);
        assert_eq!(startup_file, shell_home.path().join(".profile"));
        assert!(startup_file.is_file());
    }
}

/// The Windows half: the user's own `Path`, which is not a file.
#[cfg(windows)]
mod user_environment {
    use super::*;

    /// The user `Path` this test found, put back when the test ends — on a
    /// failing assert too, because `Drop` runs while the panic unwinds.
    /// One test at a time may touch the user `Path`.
    ///
    /// It is one registry value shared by the whole machine, so two tests
    /// editing it concurrently interleave: the second reads a `Path` the first
    /// has already replaced, and both restore whichever value they happened to
    /// read first. Holding this for the guard's life makes them run in turn.
    static USER_PATH: std::sync::Mutex<()> = std::sync::Mutex::new(());

    struct UserPathGuard {
        original: Option<String>,
        /// Held for the guard's life; released when the `Path` is restored.
        ///
        /// Poison is ignored: a test that panicked mid-edit leaves the value
        /// its own guard restores, and refusing the lock afterwards would turn
        /// one failure into every later one.
        _lock: std::sync::MutexGuard<'static, ()>,
    }

    impl UserPathGuard {
        fn take() -> Self {
            let lock = USER_PATH
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            Self {
                original: read_user_path(),
                _lock: lock,
            }
        }
    }

    impl Drop for UserPathGuard {
        fn drop(&mut self) {
            let value = self.original.clone().unwrap_or_default();
            let _ = Command::new("powershell.exe")
                .args(["-NoProfile", "-NonInteractive", "-Command"])
                .arg(
                    "[Environment]::SetEnvironmentVariable('Path', \
                     $env:KIRA_RESTORED_USER_PATH, 'User')",
                )
                .env("KIRA_RESTORED_USER_PATH", value)
                .output();
        }
    }

    /// The user's persistent `Path`, unexpanded, as knvm reads it.
    fn read_user_path() -> Option<String> {
        let output = Command::new("powershell.exe")
            .args(["-NoProfile", "-NonInteractive", "-Command"])
            .arg("[Environment]::GetEnvironmentVariable('Path', 'User')")
            .output()
            .expect("run powershell");
        assert!(output.status.success(), "read the user Path");
        let value = String::from_utf8_lossy(&output.stdout)
            .trim_end_matches(['\r', '\n'])
            .to_string();
        (!value.is_empty()).then_some(value)
    }

    /// The bin directory lands on the user's `Path` and a second run leaves it
    /// alone — the entry the user would otherwise accumulate one copy of per
    /// install.
    #[test]
    fn the_user_path_gains_the_bin_directory_once() {
        let _guard = UserPathGuard::take();
        let kira_home = TempTree::create("home");
        let shell_home = TempTree::create("shell");
        let checkout = repository_root();

        let first =
            sinstall(kira_home.path(), shell_home.path(), None, &checkout).expect("first sinstall");
        assert_eq!(
            first.path,
            PathConfigured::UserEnvironment { updated: true }
        );

        let bin_dir = first.bin_dir.display().to_string();
        let path = read_user_path().expect("a user Path after installing");
        assert!(
            path.split(';').any(|entry| entry == bin_dir),
            "the user Path must name the bin dir: {path}"
        );
        assert!(
            path.starts_with(&bin_dir),
            "the installed tools must win over an older copy: {path}"
        );

        let second = sinstall(kira_home.path(), shell_home.path(), None, &checkout)
            .expect("second sinstall");
        assert_eq!(
            second.path,
            PathConfigured::UserEnvironment { updated: false }
        );
        let after = read_user_path().expect("a user Path after re-installing");
        assert_eq!(after, path, "a second install must change nothing");
    }

    /// Whatever else was on the user's `Path` is still there afterwards — the
    /// failure mode `setx` ships, in a test rather than a bug report.
    #[test]
    fn the_rest_of_the_user_path_survives() {
        let _guard = UserPathGuard::take();
        let before = read_user_path();

        let kira_home = TempTree::create("home");
        let shell_home = TempTree::create("shell");
        sinstall(
            kira_home.path(),
            shell_home.path(),
            None,
            &repository_root(),
        )
        .expect("sinstall from this checkout");

        let after = read_user_path().expect("a user Path after installing");
        for entry in before.iter().flat_map(|path| path.split(';')) {
            assert!(
                after.split(';').any(|kept| kept == entry),
                "`{entry}` must survive the install: {after}"
            );
        }
    }
}
