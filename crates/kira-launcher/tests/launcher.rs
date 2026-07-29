//! End-to-end tests for the `kira` launcher against a fixture toolchain tree.
//!
//! Every case builds its own `KIRA_HOME` under the system temp directory and
//! passes it on the spawned child's environment only — `std::env::set_var` is
//! unsafe in edition 2024 and racy under parallel tests, so the process
//! environment is never mutated and the developer's real `~/.kira` is never
//! read. A drop guard removes the tree even when a case fails.
//!
//! The fixture `kira` is an executable `#!/bin/sh` script that echoes its own
//! marker plus every argument it received and exits with a chosen code, so a
//! passing assertion can only come from the launcher genuinely resolving
//! `current.toml` and handing the process over. Those cases are `cfg(unix)`:
//! a shell script is not executable on Windows, and this host is not Windows.

use std::path::{Path, PathBuf};
use std::process::Output;
use std::sync::atomic::{AtomicU32, Ordering};

/// A temp directory removed when the test drops it, pass or fail.
struct TempTree {
    path: PathBuf,
}

impl TempTree {
    /// Creates a uniquely-named empty directory under the system temp dir.
    fn create(label: &str) -> Self {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        let pid = std::process::id();
        let path = std::env::temp_dir().join(format!("kira_launcher_{label}_{pid}_{unique}"));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("create temp tree");
        Self { path }
    }

    /// The `KIRA_HOME` this tree stands in for.
    fn home(&self) -> &Path {
        &self.path
    }

    /// `<home>/toolchains`.
    fn toolchains(&self) -> PathBuf {
        self.path.join("toolchains")
    }
}

impl Drop for TempTree {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// Runs the built `kira` launcher with `args` and `KIRA_HOME` pointed at `home`.
///
/// The child is always waited on with its output collected, so a hung fixture
/// is impossible: the fixture script exits immediately.
fn launcher(home: &Path, args: &[&str]) -> Output {
    std::process::Command::new(env!("CARGO_BIN_EXE_kira-launcher"))
        .args(args)
        .env("KIRA_HOME", home)
        .output()
        .expect("run kira launcher")
}

/// Writes `current.toml` under `<home>/toolchains`.
fn write_current(tree: &TempTree, contents: &str) {
    let toolchains = tree.toolchains();
    std::fs::create_dir_all(&toolchains).expect("create toolchains dir");
    std::fs::write(toolchains.join("current.toml"), contents).expect("write current.toml");
}

/// Installs a fixture toolchain whose `bin/kira` echoes `marker` and every
/// argument it received, then exits with `exit_code`.
#[cfg(unix)]
fn write_fixture_toolchain(
    tree: &TempTree,
    channel: &str,
    version: &str,
    marker: &str,
    exit_code: i32,
) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;

    let bin = tree.toolchains().join(channel).join(version).join("bin");
    std::fs::create_dir_all(&bin).expect("create fixture bin dir");
    let kira = bin.join("kira");
    std::fs::write(
        &kira,
        format!(
            "#!/bin/sh\n\
             echo \"{marker}\"\n\
             for arg in \"$@\"; do echo \"arg:$arg\"; done\n\
             exit {exit_code}\n"
        ),
    )
    .expect("write fixture kira");
    std::fs::set_permissions(&kira, std::fs::Permissions::from_mode(0o755))
        .expect("mark fixture kira executable");
    kira
}

/// `current.toml` text selecting a fixture toolchain, built through the real
/// serializer so the test cannot drift from the shipped format.
fn current_toml(channel: kira_toolchain::Channel, version: &str) -> String {
    kira_toolchain::CurrentToolchain {
        channel,
        version: version.to_string(),
        primary: "kira".to_string(),
    }
    .to_toml()
}

fn stdout_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn no_toolchain_selected_reports_the_remedy_and_exits_two() {
    let tree = TempTree::create("empty");
    let output = launcher(tree.home(), &["run", "main.kira"]);
    assert_eq!(output.status.code(), Some(2));
    let stderr = stderr_of(&output);
    assert!(
        stderr.contains("no toolchain selected"),
        "stderr was: {stderr}"
    );
    assert!(
        stderr.contains("knvm install latest"),
        "stderr was: {stderr}"
    );
    assert!(stdout_of(&output).is_empty(), "launcher wrote to stdout");
}

#[test]
fn malformed_current_toml_exits_two_naming_the_file() {
    let tree = TempTree::create("malformed");
    write_current(&tree, "this is not toml at all\n");
    let output = launcher(tree.home(), &[]);
    assert_eq!(output.status.code(), Some(2));
    let stderr = stderr_of(&output);
    assert!(stderr.contains("current.toml"), "stderr was: {stderr}");
    assert!(
        stderr.contains("knvm install latest"),
        "stderr was: {stderr}"
    );
}

#[test]
fn unknown_channel_in_current_toml_exits_two() {
    let tree = TempTree::create("channel");
    write_current(
        &tree,
        "channel = \"nightly\"\nversion = \"1.0.0\"\nprimary = \"kira\"\n",
    );
    let output = launcher(tree.home(), &[]);
    assert_eq!(output.status.code(), Some(2));
    assert!(
        stderr_of(&output).contains("current.toml"),
        "stderr was: {}",
        stderr_of(&output)
    );
}

#[test]
fn selected_toolchain_without_its_binary_exits_two_naming_the_path() {
    let tree = TempTree::create("nobinary");
    write_current(
        &tree,
        &current_toml(kira_toolchain::Channel::Release, "1.2.3"),
    );
    let output = launcher(tree.home(), &[]);
    assert_eq!(output.status.code(), Some(2));
    let stderr = stderr_of(&output);
    assert!(stderr.contains("release/1.2.3"), "stderr was: {stderr}");
    assert!(stderr.contains("kira"), "stderr was: {stderr}");
    assert!(
        stderr.contains("knvm install latest"),
        "stderr was: {stderr}"
    );
}

#[cfg(unix)]
#[test]
fn dispatches_to_the_selected_toolchain_and_forwards_arguments() {
    let tree = TempTree::create("dispatch");
    write_fixture_toolchain(&tree, "release", "1.2.3", "fixture-kira-1.2.3", 0);
    write_current(
        &tree,
        &current_toml(kira_toolchain::Channel::Release, "1.2.3"),
    );

    let output = launcher(tree.home(), &["run", "--release", "main.kira"]);
    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr: {}",
        stderr_of(&output)
    );
    let stdout = stdout_of(&output);
    assert_eq!(
        stdout, "fixture-kira-1.2.3\narg:run\narg:--release\narg:main.kira\n",
        "stdout was: {stdout}"
    );
}

#[cfg(unix)]
#[test]
fn forwards_the_toolchain_exit_code() {
    let tree = TempTree::create("exitcode");
    write_fixture_toolchain(&tree, "release", "2.0.0", "fixture-kira-2.0.0", 37);
    write_current(
        &tree,
        &current_toml(kira_toolchain::Channel::Release, "2.0.0"),
    );

    let output = launcher(tree.home(), &["check"]);
    assert_eq!(output.status.code(), Some(37));
    assert!(
        stdout_of(&output).starts_with("fixture-kira-2.0.0\n"),
        "stdout was: {}",
        stdout_of(&output)
    );
}

#[cfg(unix)]
#[test]
fn arguments_with_spaces_and_leading_dashes_survive_untouched() {
    let tree = TempTree::create("argv");
    write_fixture_toolchain(&tree, "dev", "2026.07.2", "fixture-kira-dev", 0);
    write_current(
        &tree,
        &current_toml(kira_toolchain::Channel::Dev, "2026.07.2"),
    );

    let output = launcher(tree.home(), &["--", "-x", "two words", ""]);
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(
        stdout_of(&output),
        "fixture-kira-dev\narg:--\narg:-x\narg:two words\narg:\n"
    );
}

#[cfg(unix)]
#[test]
fn the_channel_recorded_in_current_toml_selects_which_toolchain_runs() {
    let tree = TempTree::create("channels");
    write_fixture_toolchain(&tree, "release", "3.0.0", "fixture-release-3.0.0", 0);
    write_fixture_toolchain(&tree, "dev", "3.0.0", "fixture-dev-3.0.0", 0);

    write_current(
        &tree,
        &current_toml(kira_toolchain::Channel::Release, "3.0.0"),
    );
    assert_eq!(
        stdout_of(&launcher(tree.home(), &[])),
        "fixture-release-3.0.0\n"
    );

    write_current(&tree, &current_toml(kira_toolchain::Channel::Dev, "3.0.0"));
    assert_eq!(
        stdout_of(&launcher(tree.home(), &[])),
        "fixture-dev-3.0.0\n"
    );
}

/// Installs a fixture `bin/kira-language-server` beside the fixture `kira`.
#[cfg(unix)]
fn write_fixture_language_server(
    tree: &TempTree,
    channel: &str,
    version: &str,
    marker: &str,
) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;

    let bin = tree.toolchains().join(channel).join(version).join("bin");
    std::fs::create_dir_all(&bin).expect("create fixture bin dir");
    let server = bin.join("kira-language-server");
    std::fs::write(&server, format!("#!/bin/sh\necho \"{marker}\"\nexit 0\n"))
        .expect("write fixture language server");
    std::fs::set_permissions(&server, std::fs::Permissions::from_mode(0o755))
        .expect("mark fixture language server executable");
    server
}

/// The launcher, invoked under the language server's name: a copy of the built
/// binary named `kira-language-server`, which is exactly what `sinstall` and
/// the tools archive put on PATH.
#[cfg(unix)]
#[test]
fn invoked_as_the_language_server_it_dispatches_to_the_toolchains_server() {
    use std::os::unix::fs::PermissionsExt;

    let tree = TempTree::create("multicall");
    write_fixture_toolchain(&tree, "release", "5.0.0", "wrong-binary-kira", 0);
    write_fixture_language_server(&tree, "release", "5.0.0", "fixture-server-5.0.0");
    write_current(
        &tree,
        &current_toml(kira_toolchain::Channel::Release, "5.0.0"),
    );

    let alias_dir = TempTree::create("multicall_alias");
    let alias = alias_dir.home().join("kira-language-server");
    std::fs::copy(env!("CARGO_BIN_EXE_kira-launcher"), &alias)
        .expect("copy launcher under alias name");
    std::fs::set_permissions(&alias, std::fs::Permissions::from_mode(0o755))
        .expect("mark alias executable");

    let output = std::process::Command::new(&alias)
        .env("KIRA_HOME", tree.home())
        .output()
        .expect("run launcher under its alias");
    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr: {}",
        stderr_of(&output)
    );
    assert_eq!(
        stdout_of(&output),
        "fixture-server-5.0.0\n",
        "the alias must dispatch to the server, not the primary"
    );
}

/// A toolchain with no server is a dispatch error under the alias, while the
/// same tree still dispatches fine under the primary name.
#[cfg(unix)]
#[test]
fn the_alias_reports_a_toolchain_without_a_server_and_exits_two() {
    use std::os::unix::fs::PermissionsExt;

    let tree = TempTree::create("multicall_missing");
    write_fixture_toolchain(&tree, "release", "6.0.0", "fixture-kira-6.0.0", 0);
    write_current(
        &tree,
        &current_toml(kira_toolchain::Channel::Release, "6.0.0"),
    );

    let alias_dir = TempTree::create("multicall_missing_alias");
    let alias = alias_dir.home().join("kira-language-server");
    std::fs::copy(env!("CARGO_BIN_EXE_kira-launcher"), &alias)
        .expect("copy launcher under alias name");
    std::fs::set_permissions(&alias, std::fs::Permissions::from_mode(0o755))
        .expect("mark alias executable");

    let output = std::process::Command::new(&alias)
        .env("KIRA_HOME", tree.home())
        .output()
        .expect("run launcher under its alias");
    assert_eq!(output.status.code(), Some(2));
    assert!(
        stderr_of(&output).contains("kira-language-server"),
        "stderr was: {}",
        stderr_of(&output)
    );

    let primary = launcher(tree.home(), &[]);
    assert_eq!(
        stdout_of(&primary),
        "fixture-kira-6.0.0\n",
        "the primary dispatch must be untouched by the missing server"
    );
}

#[cfg(unix)]
#[test]
fn a_non_executable_primary_binary_is_reported_rather_than_dispatched() {
    let tree = TempTree::create("notexec");
    use std::os::unix::fs::PermissionsExt;
    let kira = write_fixture_toolchain(&tree, "release", "4.0.0", "unreachable", 0);
    std::fs::set_permissions(&kira, std::fs::Permissions::from_mode(0o644))
        .expect("clear the executable bit");
    write_current(
        &tree,
        &current_toml(kira_toolchain::Channel::Release, "4.0.0"),
    );

    let output = launcher(tree.home(), &[]);
    assert_eq!(output.status.code(), Some(2));
    assert!(
        stderr_of(&output).contains("cannot execute"),
        "stderr was: {}",
        stderr_of(&output)
    );
    assert!(
        !stdout_of(&output).contains("unreachable"),
        "the fixture must not have run"
    );
}
