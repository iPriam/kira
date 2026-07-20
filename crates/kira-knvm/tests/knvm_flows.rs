//! End-to-end install flows driven against a fixture release directory.
//!
//! Nothing here touches the developer's real `~/.kira`: every test builds its
//! own tree under the system temp directory, hands that path to the library as
//! an explicit toolchains root, and removes it on drop. Nothing here opens a
//! network connection either — the release feed is a directory of real
//! `.tar.gz` archives built by the test with the system `tar`, so extraction,
//! validation, and the move into place are genuinely exercised. Only the
//! transport is substituted.

use std::path::Path;

use kira_knvm::{
    Channel, DirectoryReleaseSource, InstallError, VersionSpec, install, read_current,
};

mod support;
use support::{FixtureToolchain, TempTree, host_key, publish};

/// Whether a path carries an executable bit.
fn is_executable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path)
            .map(|metadata| metadata.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        path.is_file()
    }
}

#[test]
fn install_latest_lays_out_the_toolchain_and_selects_it() {
    let releases = TempTree::create("feed");
    let home = TempTree::create("home");
    publish(
        releases.path(),
        Channel::Release,
        "1.7.3",
        &FixtureToolchain::complete(),
    );
    publish(
        releases.path(),
        Channel::Release,
        "1.10.0",
        &FixtureToolchain::complete(),
    );

    let source = DirectoryReleaseSource::new(releases.path()).expect("supported host");
    let installed = install(home.path(), &source, &VersionSpec::Latest, Channel::Release)
        .expect("install latest");

    assert_eq!(
        installed.version, "1.10.0",
        "`latest` must order versions numerically, not lexicographically"
    );
    assert!(!installed.already_installed);
    assert_eq!(
        installed.root,
        home.path().join("release").join("1.10.0"),
        "the toolchain must land under <root>/<channel>/<version>"
    );

    let binary = installed.root.join("bin").join("kirac");
    assert!(binary.is_file(), "bin/kirac must be installed");
    assert!(
        is_executable(&binary),
        "bin/kirac must keep its executable bit through archive and extract"
    );
    assert!(
        installed
            .root
            .join("foundation")
            .join("package.kira")
            .is_file(),
        "the bundled Foundation package must sit beside bin/"
    );
    assert!(
        installed
            .root
            .join("foundation")
            .join("app")
            .join("foundation.kira")
            .is_file(),
        "Foundation's sources must survive the install"
    );
    assert!(installed.root.join("templates").join("app").is_dir());

    let current = read_current(home.path())
        .expect("read current.toml")
        .expect("install must select what it installed");
    assert_eq!(current.channel, Channel::Release);
    assert_eq!(current.version, "1.10.0");
    assert_eq!(current.primary, "kirac");

    assert!(
        !staging_has_leftovers(home.path()),
        "staging must not survive a successful install"
    );
}

#[test]
fn installing_an_exact_version_on_the_dev_channel_keeps_the_channels_apart() {
    let releases = TempTree::create("feed");
    let home = TempTree::create("home");
    publish(
        releases.path(),
        Channel::Release,
        "1.7.3",
        &FixtureToolchain::complete(),
    );
    publish(
        releases.path(),
        Channel::Dev,
        "2026.07.2",
        &FixtureToolchain::complete(),
    );

    let source = DirectoryReleaseSource::new(releases.path()).expect("supported host");
    install(
        home.path(),
        &source,
        &VersionSpec::Exact("1.7.3".to_string()),
        Channel::Release,
    )
    .expect("install the release build");
    let dev = install(
        home.path(),
        &source,
        &VersionSpec::Exact("2026.07.2".to_string()),
        Channel::Dev,
    )
    .expect("install the dev build");

    assert_eq!(dev.root, home.path().join("dev").join("2026.07.2"));
    assert!(
        home.path().join("release").join("1.7.3").is_dir(),
        "installing on one channel must not disturb the other"
    );

    let current = read_current(home.path())
        .expect("read current.toml")
        .expect("a toolchain is selected");
    assert_eq!(current.channel, Channel::Dev);
    assert_eq!(current.version, "2026.07.2");
}

#[test]
fn reinstalling_an_installed_version_reselects_it_without_refetching() {
    let releases = TempTree::create("feed");
    let home = TempTree::create("home");
    publish(
        releases.path(),
        Channel::Release,
        "1.7.3",
        &FixtureToolchain::complete(),
    );

    let source = DirectoryReleaseSource::new(releases.path()).expect("supported host");
    let spec = VersionSpec::Exact("1.7.3".to_string());
    let first = install(home.path(), &source, &spec, Channel::Release).expect("first install");
    assert!(!first.already_installed);

    // A marker file proves the tree was left alone rather than replaced.
    let marker = first.root.join("installed-once");
    std::fs::write(&marker, "witness").expect("write marker");

    let second = install(home.path(), &source, &spec, Channel::Release).expect("second install");
    assert!(second.already_installed);
    assert!(
        marker.is_file(),
        "an already-installed version must not be re-extracted over"
    );
    assert_eq!(
        read_current(home.path())
            .expect("read current.toml")
            .expect("still selected")
            .version,
        "1.7.3"
    );
}

#[test]
fn an_archive_without_a_primary_binary_is_refused_and_leaves_nothing_behind() {
    let releases = TempTree::create("feed");
    let home = TempTree::create("home");
    publish(
        releases.path(),
        Channel::Release,
        "1.7.3",
        &FixtureToolchain {
            with_primary_binary: false,
            with_foundation: true,
        },
    );

    let source = DirectoryReleaseSource::new(releases.path()).expect("supported host");
    let error = install(
        home.path(),
        &source,
        &VersionSpec::Exact("1.7.3".to_string()),
        Channel::Release,
    )
    .expect_err("a toolchain with no kirac is not installable");
    assert!(
        matches!(error, InstallError::MissingPrimaryBinary { .. }),
        "expected a typed validation refusal, got: {error}"
    );

    assert!(
        !home.path().join("release").join("1.7.3").exists(),
        "a refused install must leave no partial toolchain"
    );
    assert_eq!(
        read_current(home.path()).expect("read current.toml"),
        None,
        "a refused install must select nothing"
    );
    assert!(
        !staging_has_leftovers(home.path()),
        "a refused install must clean its staging directory"
    );
}

#[test]
fn an_archive_without_foundation_is_refused() {
    let releases = TempTree::create("feed");
    let home = TempTree::create("home");
    publish(
        releases.path(),
        Channel::Release,
        "1.7.3",
        &FixtureToolchain {
            with_primary_binary: true,
            with_foundation: false,
        },
    );

    let source = DirectoryReleaseSource::new(releases.path()).expect("supported host");
    let error = install(
        home.path(),
        &source,
        &VersionSpec::Exact("1.7.3".to_string()),
        Channel::Release,
    )
    .expect_err("a toolchain with no bundled Foundation is not installable");
    assert!(
        matches!(error, InstallError::MissingFoundation { .. }),
        "expected a typed validation refusal, got: {error}"
    );
    assert!(!home.path().join("release").join("1.7.3").exists());
}

#[test]
fn a_version_the_feed_does_not_have_is_refused_by_name() {
    let releases = TempTree::create("feed");
    let home = TempTree::create("home");
    publish(
        releases.path(),
        Channel::Release,
        "1.7.3",
        &FixtureToolchain::complete(),
    );

    let source = DirectoryReleaseSource::new(releases.path()).expect("supported host");
    let error = install(
        home.path(),
        &source,
        &VersionSpec::Exact("9.9.9".to_string()),
        Channel::Release,
    )
    .expect_err("an unpublished version cannot be installed");
    let rendered = error.to_string();
    assert!(
        rendered.contains("9.9.9"),
        "the error must name the version asked for: {rendered}"
    );
    assert!(!home.path().join("release").exists());
}

#[test]
fn install_never_touches_the_llvm_and_libffi_subtrees() {
    let releases = TempTree::create("feed");
    let home = TempTree::create("home");
    publish(
        releases.path(),
        Channel::Release,
        "1.7.3",
        &FixtureToolchain::complete(),
    );

    // Pre-existing shared bundles, exactly as a machine with a provisioned LLVM
    // has them. A toolchain install must be blind to them.
    let llvm = home.path().join("llvm").join("22.1.4").join(host_key());
    std::fs::create_dir_all(llvm.join("bin")).expect("create llvm bundle");
    std::fs::write(llvm.join("bin").join("clang"), "witness").expect("write clang witness");
    let libffi = home.path().join("libffi").join("3.5.2").join(host_key());
    std::fs::create_dir_all(&libffi).expect("create libffi bundle");
    std::fs::write(libffi.join("witness"), "witness").expect("write libffi witness");

    let source = DirectoryReleaseSource::new(releases.path()).expect("supported host");
    install(home.path(), &source, &VersionSpec::Latest, Channel::Release).expect("install latest");

    assert_eq!(
        std::fs::read_to_string(llvm.join("bin").join("clang")).expect("clang witness survives"),
        "witness"
    );
    assert_eq!(
        std::fs::read_to_string(libffi.join("witness")).expect("libffi witness survives"),
        "witness"
    );
}

#[test]
fn an_empty_channel_is_reported_rather_than_installed_from() {
    let releases = TempTree::create("feed");
    let home = TempTree::create("home");
    publish(
        releases.path(),
        Channel::Release,
        "1.7.3",
        &FixtureToolchain::complete(),
    );

    let source = DirectoryReleaseSource::new(releases.path()).expect("supported host");
    let error = install(home.path(), &source, &VersionSpec::Latest, Channel::Dev)
        .expect_err("the dev channel publishes nothing in this feed");
    let rendered = error.to_string();
    assert!(
        rendered.contains("dev"),
        "the error must name the empty channel: {rendered}"
    );
}

#[test]
fn a_version_that_is_not_a_single_component_is_refused_by_install() {
    let releases = TempTree::create("feed");
    let home = TempTree::create("home");
    publish(
        releases.path(),
        Channel::Release,
        "1.7.3",
        &FixtureToolchain::complete(),
    );

    // A sibling of the toolchains root, standing in for anything an escaping
    // version could reach. `uninstall` already refuses these; `install` joins
    // the version onto the root just the same, so it must refuse them too.
    let outsider = home.path().parent().expect("temp root has a parent");
    let source = DirectoryReleaseSource::new(releases.path()).expect("supported host");

    for escape in ["..", "../escape", "release/../..", "", "."] {
        let error = install(
            home.path(),
            &source,
            &VersionSpec::Exact(escape.to_string()),
            Channel::Release,
        )
        .expect_err("a version that is not one directory component cannot be installed");
        assert!(
            matches!(error, InstallError::InvalidVersion { .. }),
            "`{escape}` must be refused as a version name, got: {error}"
        );
    }

    assert!(
        !outsider.join("escape").exists(),
        "a refused version must not have created anything outside the toolchains root"
    );
    assert!(!staging_has_leftovers(home.path()));
}

#[test]
fn an_occupied_but_unusable_destination_is_refused_rather_than_selected() {
    let releases = TempTree::create("feed");
    let home = TempTree::create("home");
    publish(
        releases.path(),
        Channel::Release,
        "1.7.3",
        &FixtureToolchain::complete(),
    );

    // A destination that exists but holds no runnable toolchain: an interrupted
    // hand-copy, or a tree someone emptied. Reporting this as installed would
    // select a toolchain the launcher cannot dispatch to.
    let destination = home.path().join("release").join("1.7.3");
    std::fs::create_dir_all(destination.join("bin")).expect("create the damaged tree");

    let source = DirectoryReleaseSource::new(releases.path()).expect("supported host");
    let error = install(
        home.path(),
        &source,
        &VersionSpec::Exact("1.7.3".to_string()),
        Channel::Release,
    )
    .expect_err("an unusable install cannot be reported as already installed");
    assert!(
        matches!(error, InstallError::IncompleteInstall { .. }),
        "a damaged destination must be named as such, got: {error}"
    );

    assert!(
        read_current(home.path())
            .expect("read current.toml")
            .is_none(),
        "a refused install must not select the damaged toolchain"
    );
    assert!(!staging_has_leftovers(home.path()));
}

#[test]
fn a_malformed_selection_is_reported_rather_than_read_as_nothing_selected() {
    let home = TempTree::create("home");
    std::fs::create_dir_all(home.path()).expect("create the toolchains root");
    std::fs::write(
        home.path().join("current.toml"),
        "channel = \"release\"\nversion =\n",
    )
    .expect("write a damaged selection");

    // Reading this as `None` would have knvm report nothing selected while the
    // launcher, reading the same file, refuses to dispatch.
    let error = read_current(home.path()).expect_err("a damaged selection is not `nothing`");
    assert!(
        matches!(error, InstallError::MalformedCurrent { .. }),
        "a damaged current.toml must be named as such, got: {error}"
    );
}

/// Whether any staging directory survived under a toolchains root.
fn staging_has_leftovers(toolchains_root: &Path) -> bool {
    let staging = toolchains_root.join(".staging");
    match std::fs::read_dir(&staging) {
        Ok(entries) => entries.count() > 0,
        Err(_) => false,
    }
}
