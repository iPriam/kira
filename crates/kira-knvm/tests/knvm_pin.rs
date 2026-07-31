//! Per-project pinning, end to end: a pin file decides which installed
//! toolchain a directory tree's `kira` runs.
//!
//! The launcher is driven as a real process against a throwaway `KIRA_HOME`,
//! because that is the only way to prove the thing that matters — that the pin
//! beats the global selection *in the shipped binary*, not in a copy of its
//! resolution logic.

use std::path::Path;
use std::process::Command;

use kira_knvm::{Channel, DirectoryReleaseSource, VersionSpec, install};
use kira_toolchain::{PinnedToolchain, find_pin, write_pin};

mod support;
use support::{FixtureToolchain, TempTree, publish};

/// The launcher as cargo built it, beside this test binary.
///
/// Absent when the test is run without the launcher built; the tests that
/// need it say so and skip, rather than passing on a launcher nobody ran.
fn launcher() -> Option<std::path::PathBuf> {
    let mut directory = std::env::current_exe().ok()?;
    directory.pop(); // the deps/ directory
    directory.pop(); // the profile directory
    let path = directory.join(kira_toolchain::executable_name("kira-launcher"));
    path.is_file().then_some(path)
}

/// Installs two versions into a throwaway home and selects the first.
fn two_installed(home: &Path, releases: &Path) {
    publish(
        releases,
        Channel::Release,
        "1.7.3",
        &FixtureToolchain::complete(),
    );
    publish(
        releases,
        Channel::Release,
        "1.10.0",
        &FixtureToolchain::complete(),
    );
    let source = DirectoryReleaseSource::new(releases).expect("supported host");
    install(
        home,
        &source,
        &VersionSpec::Exact("1.10.0".to_string()),
        Channel::Release,
    )
    .expect("install the newer version");
    // Installed last, so `current.toml` selects it: the pin below names the
    // other one, and the test is which of the two actually runs.
    install(
        home,
        &source,
        &VersionSpec::Exact("1.7.3".to_string()),
        Channel::Release,
    )
    .expect("install and select the older version");
}

#[test]
fn a_pin_beats_the_global_selection() {
    let Some(launcher) = launcher() else {
        eprintln!("kira-launcher is not built beside this test; skipping");
        return;
    };
    let releases = TempTree::create("pin_feed");
    let kira_home = TempTree::create("pin_home");
    let project = TempTree::create("pin_project");

    let toolchains = kira_home.path().join("toolchains");
    two_installed(&toolchains, releases.path());

    write_pin(
        project.path(),
        &PinnedToolchain {
            channel: Channel::Release,
            version: "1.10.0".to_string(),
            path: std::path::PathBuf::new(),
        },
    )
    .expect("write the pin");

    let output = Command::new(&launcher)
        .arg("--version")
        .current_dir(project.path())
        .env("KIRA_HOME", kira_home.path())
        .output()
        .expect("run the launcher");
    let said = String::from_utf8_lossy(&output.stdout);
    assert!(
        said.contains("kira 1.10.0"),
        "the pinned toolchain must run, not the selected one; got {said:?}"
    );

    // The same launcher, one directory outside the pinned tree, follows the
    // global selection again.
    let output = Command::new(&launcher)
        .arg("--version")
        .current_dir(kira_home.path())
        .env("KIRA_HOME", kira_home.path())
        .output()
        .expect("run the launcher outside the pinned tree");
    let said = String::from_utf8_lossy(&output.stdout);
    assert!(
        said.contains("kira 1.7.3"),
        "outside the pin, the selected toolchain must run; got {said:?}"
    );
}

#[test]
fn a_pin_naming_an_uninstalled_version_refuses_rather_than_falling_back() {
    let Some(launcher) = launcher() else {
        eprintln!("kira-launcher is not built beside this test; skipping");
        return;
    };
    let releases = TempTree::create("badpin_feed");
    let kira_home = TempTree::create("badpin_home");
    let project = TempTree::create("badpin_project");

    let toolchains = kira_home.path().join("toolchains");
    two_installed(&toolchains, releases.path());

    write_pin(
        project.path(),
        &PinnedToolchain {
            channel: Channel::Release,
            version: "9.9.9".to_string(),
            path: std::path::PathBuf::new(),
        },
    )
    .expect("write the pin");

    let output = Command::new(&launcher)
        .arg("--version")
        .current_dir(project.path())
        .env("KIRA_HOME", kira_home.path())
        .output()
        .expect("run the launcher");
    assert_eq!(
        output.status.code(),
        Some(2),
        "the launcher must report that it could not dispatch"
    );
    let complaint = String::from_utf8_lossy(&output.stderr);
    assert!(
        complaint.contains("9.9.9") && complaint.contains("not installed"),
        "the refusal must name the pinned version; got {complaint:?}"
    );
    assert!(
        !complaint.contains("1.7.3"),
        "a pin must never silently fall back to the selected toolchain"
    );
}

#[test]
fn a_malformed_pin_is_refused_rather_than_ignored() {
    let Some(launcher) = launcher() else {
        eprintln!("kira-launcher is not built beside this test; skipping");
        return;
    };
    let releases = TempTree::create("junkpin_feed");
    let kira_home = TempTree::create("junkpin_home");
    let project = TempTree::create("junkpin_project");

    let toolchains = kira_home.path().join("toolchains");
    two_installed(&toolchains, releases.path());

    std::fs::write(
        project.path().join(kira_toolchain::PIN_FILE_NAME),
        "channel = \"release\"\n",
    )
    .expect("write a pin with no version");

    let output = Command::new(&launcher)
        .arg("--version")
        .current_dir(project.path())
        .env("KIRA_HOME", kira_home.path())
        .output()
        .expect("run the launcher");
    assert_eq!(output.status.code(), Some(2));
    let complaint = String::from_utf8_lossy(&output.stderr);
    assert!(
        complaint.contains("not a toolchain pin"),
        "the refusal must say what is wrong with the file; got {complaint:?}"
    );
}

#[test]
fn the_nearest_pin_governs_a_nested_project() {
    let outer = TempTree::create("nested_pin");
    let inner = outer.path().join("packages").join("app");
    std::fs::create_dir_all(&inner).expect("create the nested tree");

    for (directory, version) in [(outer.path(), "1.7.3"), (inner.as_path(), "1.10.0")] {
        write_pin(
            directory,
            &PinnedToolchain {
                channel: Channel::Release,
                version: version.to_string(),
                path: std::path::PathBuf::new(),
            },
        )
        .expect("write a pin");
    }

    let found = find_pin(&inner)
        .expect("a readable pin")
        .expect("the nested tree is pinned");
    assert_eq!(found.version, "1.10.0");
}
