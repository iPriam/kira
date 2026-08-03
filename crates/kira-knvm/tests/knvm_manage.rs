//! End-to-end `list`, `use`, and `uninstall` flows.
//!
//! Every toolchain these operate on is genuinely installed first, by the shipped
//! [`install`] path against a fixture release feed of real `tar` archives — so
//! what is listed, selected, and removed is a real installed tree and not a
//! directory a test hand-carved to look like one. Nothing here reads the
//! developer's `~/.kira`: each test passes its own temp directory as the
//! toolchains root, and the tree removes itself on drop.

use std::path::Path;

use kira_knvm::{
    Channel, DirectoryReleaseSource, ManageError, VersionSpec, install, list, read_current, select,
    uninstall,
};
use kira_toolchain::executable_name;

mod support;
use support::{FixtureToolchain, TempTree, host_key, publish};

/// A home with `release` 1.7.3 and 1.10.0 and `dev` 2026.07.2 installed, with
/// `release` 1.10.0 selected last. Returns the feed and home trees, which must
/// both stay alive for the duration of the test.
fn installed_home() -> (TempTree, TempTree) {
    let releases = TempTree::create("feed");
    let home = TempTree::create("home");
    for (channel, version) in [
        (Channel::Release, "1.7.3"),
        (Channel::Release, "1.10.0"),
        (Channel::Dev, "2026.07.2"),
    ] {
        publish(
            releases.path(),
            channel,
            version,
            &FixtureToolchain::complete(),
        );
    }

    let source = DirectoryReleaseSource::new(releases.path()).expect("supported host");
    for (channel, version) in [
        (Channel::Release, "1.7.3"),
        (Channel::Dev, "2026.07.2"),
        (Channel::Release, "1.10.0"),
    ] {
        install(
            home.path(),
            &source,
            &VersionSpec::Exact(version.to_string()),
            channel,
        )
        .expect("install the fixture toolchain");
    }

    (releases, home)
}

/// The version of the currently selected toolchain, if any.
fn selected_version(toolchains_root: &Path) -> Option<String> {
    read_current(toolchains_root)
        .expect("read current.toml")
        .map(|current| current.version)
}

#[test]
fn list_groups_by_channel_newest_first_and_marks_the_selected_one() {
    let (_releases, home) = installed_home();

    let installed = list(home.path()).expect("list installed toolchains");
    let summary: Vec<(&str, &str, bool)> = installed
        .iter()
        .map(|entry| {
            (
                entry.channel.dir_name(),
                entry.version.as_str(),
                entry.is_current,
            )
        })
        .collect();

    assert_eq!(
        summary,
        [
            ("release", "1.10.0", true),
            ("release", "1.7.3", false),
            ("dev", "2026.07.2", false),
        ],
        "release comes first, versions are newest first, and exactly the \
         last-installed toolchain is marked current"
    );

    for entry in &installed {
        assert_eq!(
            entry.root,
            home.path()
                .join(entry.channel.dir_name())
                .join(&entry.version),
            "a listed root must be the directory the launcher dispatches into"
        );
        assert!(
            entry.is_complete,
            "an installed toolchain ships bin/kira: {}",
            entry.version
        );
    }
}

#[test]
fn list_reports_nothing_for_a_root_with_no_toolchains_and_ignores_the_shared_bundles() {
    let home = TempTree::create("home");

    // `llvm/` and `libffi/` are siblings of the channel directories. A listing
    // walks channels, so these must not surface as toolchains.
    std::fs::create_dir_all(home.path().join("llvm").join("22.1.4").join(host_key()))
        .expect("create llvm bundle");
    std::fs::create_dir_all(home.path().join("libffi").join("3.5.2").join(host_key()))
        .expect("create libffi bundle");

    assert_eq!(
        list(home.path()).expect("an empty root lists nothing"),
        &[],
        "only installed toolchains are toolchains"
    );
}

#[test]
fn list_shows_a_toolchain_that_lost_its_binary_as_broken_rather_than_hiding_it() {
    let (_releases, home) = installed_home();
    std::fs::remove_file(
        home.path()
            .join("release")
            .join("1.7.3")
            .join("bin")
            .join(executable_name("kira")),
    )
    .expect("break the installed toolchain");

    let broken = list(home.path())
        .expect("list installed toolchains")
        .into_iter()
        .find(|entry| entry.version == "1.7.3")
        .expect("a tree with a missing binary is still installed");
    assert!(
        !broken.is_complete,
        "a toolchain the launcher cannot dispatch to must be reported, not hidden"
    );
}

#[test]
fn use_reselects_an_installed_version_without_disturbing_the_trees() {
    let (_releases, home) = installed_home();
    assert_eq!(selected_version(home.path()).as_deref(), Some("1.10.0"));

    let selected = select(home.path(), Channel::Release, "1.7.3").expect("select 1.7.3");
    assert!(!selected.was_already_current);
    assert_eq!(selected.root, home.path().join("release").join("1.7.3"));

    let current = read_current(home.path())
        .expect("read current.toml")
        .expect("a toolchain is selected");
    assert_eq!(current.channel, Channel::Release);
    assert_eq!(current.version, "1.7.3");
    assert_eq!(current.primary, "kira");

    assert!(
        home.path().join("release").join("1.10.0").is_dir(),
        "selecting one version must not remove another"
    );

    let marked: Vec<String> = list(home.path())
        .expect("list")
        .into_iter()
        .filter(|entry| entry.is_current)
        .map(|entry| entry.version)
        .collect();
    assert_eq!(marked, ["1.7.3"], "exactly one toolchain is ever current");
}

#[test]
fn use_crosses_channels_and_reports_a_no_op_reselection() {
    let (_releases, home) = installed_home();

    let dev = select(home.path(), Channel::Dev, "2026.07.2").expect("select the dev build");
    assert!(!dev.was_already_current);
    assert_eq!(
        read_current(home.path())
            .expect("read current.toml")
            .expect("selected")
            .channel,
        Channel::Dev,
        "current.toml records the channel, so the launcher needs no cross-channel search"
    );

    let again = select(home.path(), Channel::Dev, "2026.07.2").expect("reselect the dev build");
    assert!(
        again.was_already_current,
        "reselecting what is already selected is a reported no-op, not a failure"
    );
}

#[test]
fn use_refuses_a_version_that_is_not_installed_and_leaves_the_selection_alone() {
    let (_releases, home) = installed_home();

    let error = select(home.path(), Channel::Release, "9.9.9")
        .expect_err("an uninstalled version cannot be selected");
    assert!(
        matches!(error, ManageError::NotInstalled { .. }),
        "expected a typed refusal, got: {error}"
    );
    assert!(error.to_string().contains("9.9.9"));

    // Installed on `release`, asked for on `dev`: the channels are separate
    // installs, so this must be refused rather than found by searching.
    let error =
        select(home.path(), Channel::Dev, "1.10.0").expect_err("channels are independent installs");
    assert!(matches!(error, ManageError::NotInstalled { .. }));

    assert_eq!(
        selected_version(home.path()).as_deref(),
        Some("1.10.0"),
        "a refused selection must not change what is selected"
    );
}

#[test]
fn use_refuses_an_installed_tree_that_cannot_be_dispatched_to() {
    let (_releases, home) = installed_home();
    std::fs::remove_file(
        home.path()
            .join("release")
            .join("1.7.3")
            .join("bin")
            .join(executable_name("kira")),
    )
    .expect("break the installed toolchain");

    let error = select(home.path(), Channel::Release, "1.7.3")
        .expect_err("selecting a tree with no kira would leave `kira` unable to dispatch");
    assert!(
        matches!(error, ManageError::Incomplete { .. }),
        "expected a typed refusal, got: {error}"
    );
    assert_eq!(
        selected_version(home.path()).as_deref(),
        Some("1.10.0"),
        "the previous selection must survive a refused one"
    );
}

#[test]
fn uninstall_removes_exactly_one_version_directory() {
    let (_releases, home) = installed_home();

    let removed = uninstall(home.path(), Channel::Release, "1.7.3").expect("uninstall 1.7.3");
    assert!(
        !removed.was_current,
        "1.10.0 was selected, so removing 1.7.3 changes no selection"
    );
    assert_eq!(removed.root, home.path().join("release").join("1.7.3"));
    assert!(!removed.root.exists(), "the version directory must be gone");

    assert!(
        home.path().join("release").join("1.10.0").is_dir(),
        "a sibling version on the same channel must survive"
    );
    assert!(
        home.path().join("dev").join("2026.07.2").is_dir(),
        "the other channel must survive"
    );
    assert_eq!(
        selected_version(home.path()).as_deref(),
        Some("1.10.0"),
        "removing a version that was not selected must not clear the selection"
    );

    let remaining: Vec<String> = list(home.path())
        .expect("list")
        .into_iter()
        .map(|entry| entry.version)
        .collect();
    assert_eq!(remaining, ["1.10.0", "2026.07.2"]);
}

#[test]
fn uninstalling_the_selected_version_clears_the_selection() {
    let (_releases, home) = installed_home();

    let removed = uninstall(home.path(), Channel::Release, "1.10.0").expect("uninstall 1.10.0");
    assert!(
        removed.was_current,
        "the caller must be told the selection is gone so it can warn"
    );
    assert_eq!(
        read_current(home.path()).expect("read current.toml"),
        None,
        "clearing is deliberate: guessing a replacement would silently change \
         which compiler runs"
    );
    assert!(
        home.path().join("release").join("1.7.3").is_dir(),
        "the other versions stay installed and can be selected again"
    );

    select(home.path(), Channel::Release, "1.7.3").expect("recover by selecting another version");
    assert_eq!(selected_version(home.path()).as_deref(), Some("1.7.3"));
}

#[test]
fn uninstall_refuses_a_version_that_is_not_installed() {
    let (_releases, home) = installed_home();

    let error = uninstall(home.path(), Channel::Release, "9.9.9")
        .expect_err("an uninstalled version cannot be removed");
    assert!(
        matches!(error, ManageError::NotInstalled { .. }),
        "expected a typed refusal, got: {error}"
    );
    assert_eq!(
        list(home.path()).expect("list").len(),
        3,
        "a refused uninstall must remove nothing"
    );
}

#[test]
fn uninstall_never_reaches_the_shared_bundles_or_another_run_s_staging() {
    let (_releases, home) = installed_home();

    let llvm = home.path().join("llvm").join("22.1.4").join(host_key());
    std::fs::create_dir_all(&llvm).expect("create llvm bundle");
    std::fs::write(llvm.join("witness"), "witness").expect("write llvm witness");
    let libffi = home.path().join("libffi").join("3.5.2").join(host_key());
    std::fs::create_dir_all(&libffi).expect("create libffi bundle");
    std::fs::write(libffi.join("witness"), "witness").expect("write libffi witness");
    let staging = home.path().join(".staging").join("99999-0");
    std::fs::create_dir_all(&staging).expect("create a concurrent run's staging");
    std::fs::write(staging.join("witness"), "witness").expect("write staging witness");

    // A version argument that tries to walk out of its channel directory is
    // refused before any path is joined, let alone removed.
    for escape in ["../../llvm", "..", "../libffi"] {
        let error = uninstall(home.path(), Channel::Release, escape)
            .expect_err("a traversing version must be refused");
        assert!(
            matches!(error, ManageError::InvalidVersion { .. }),
            "expected `{escape}` to be refused as not a version name, got: {error}"
        );
    }

    uninstall(home.path(), Channel::Release, "1.7.3").expect("a real uninstall still works");

    for witness in [
        llvm.join("witness"),
        libffi.join("witness"),
        staging.join("witness"),
    ] {
        assert_eq!(
            std::fs::read_to_string(&witness).unwrap_or_default(),
            "witness",
            "uninstall must not touch `{}`",
            witness.display()
        );
    }
}
