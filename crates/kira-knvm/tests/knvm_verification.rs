//! What a published checksum is worth: an install that verifies, one that
//! refuses, and one that honestly reports having verified nothing.
//!
//! Driven against the same fixture release directory as the other flows, so
//! the code path under test is the shipped one with only the transport
//! substituted. The digests here are computed over real archives by the same
//! function the installer uses, so a test passing means the two agree — not
//! that both were handed the same constant.

use kira_knvm::{
    Channel, DirectoryReleaseSource, InstallError, ReleaseSource, ReleaseSourceError, Sha256,
    VersionSpec, install,
};
use kira_toolchain::executable_name;

mod support;
use support::{FixtureToolchain, TempTree, checksum_sidecar_path, publish};

#[test]
fn a_published_checksum_is_verified_and_reported() {
    let releases = TempTree::create("verify_feed");
    let home = TempTree::create("verify_home");
    publish(
        releases.path(),
        Channel::Release,
        "1.10.0",
        &FixtureToolchain::complete(),
    );

    let source = DirectoryReleaseSource::new(releases.path()).expect("supported host");
    let installed = install(home.path(), &source, &VersionSpec::Latest, Channel::Release)
        .expect("a matching checksum installs");

    let digest = installed
        .verified
        .expect("the fixture publishes a sidecar, so the install is verified");
    let published = source
        .published_checksum(Channel::Release, "1.10.0")
        .expect("the sidecar is readable")
        .expect("the sidecar exists");
    assert_eq!(
        digest, published,
        "the digest reported must be the one the release published"
    );
}

#[test]
fn a_mismatched_checksum_refuses_and_installs_nothing() {
    let releases = TempTree::create("mismatch_feed");
    let home = TempTree::create("mismatch_home");
    publish(
        releases.path(),
        Channel::Release,
        "1.10.0",
        &FixtureToolchain::complete(),
    );

    // A digest of different bytes: what a corrupted transfer or a substituted
    // archive would produce.
    let wrong = Sha256::of(b"not the bytes that were published");
    std::fs::write(
        checksum_sidecar_path(releases.path(), Channel::Release, "1.10.0"),
        format!("{wrong}\n"),
    )
    .expect("rewrite the sidecar");

    let source = DirectoryReleaseSource::new(releases.path()).expect("supported host");
    let error = install(home.path(), &source, &VersionSpec::Latest, Channel::Release)
        .expect_err("a mismatched checksum must refuse");

    match error {
        InstallError::ChecksumMismatch { expected, .. } => assert_eq!(expected, wrong),
        other => panic!("expected a checksum refusal, got {other}"),
    }

    assert!(
        !home.path().join("release").join("1.10.0").exists(),
        "a refused install must leave no toolchain behind"
    );
    assert_eq!(
        kira_knvm::read_current(home.path()).expect("readable"),
        None,
        "a refused install must select nothing"
    );
}

#[test]
fn an_unreadable_sidecar_is_a_refusal_rather_than_an_unverified_install() {
    let releases = TempTree::create("garbage_feed");
    let home = TempTree::create("garbage_home");
    publish(
        releases.path(),
        Channel::Release,
        "1.10.0",
        &FixtureToolchain::complete(),
    );
    std::fs::write(
        checksum_sidecar_path(releases.path(), Channel::Release, "1.10.0"),
        "this is not a digest\n",
    )
    .expect("rewrite the sidecar");

    let source = DirectoryReleaseSource::new(releases.path()).expect("supported host");
    let error = install(home.path(), &source, &VersionSpec::Latest, Channel::Release)
        .expect_err("a sidecar that cannot be read is a publishing failure");
    assert!(
        matches!(
            error,
            InstallError::Source(ReleaseSourceError::MalformedChecksum { .. })
        ),
        "expected a malformed-checksum refusal, got {error}"
    );
    assert!(!home.path().join("release").join("1.10.0").exists());
}

#[test]
fn a_release_without_a_sidecar_installs_and_says_it_was_not_verified() {
    let releases = TempTree::create("nosidecar_feed");
    let home = TempTree::create("nosidecar_home");
    publish(
        releases.path(),
        Channel::Release,
        "1.10.0",
        &FixtureToolchain::complete(),
    );
    std::fs::remove_file(checksum_sidecar_path(
        releases.path(),
        Channel::Release,
        "1.10.0",
    ))
    .expect("remove the sidecar");

    let source = DirectoryReleaseSource::new(releases.path()).expect("supported host");
    assert_eq!(
        source
            .published_checksum(Channel::Release, "1.10.0")
            .expect("an absent sidecar is not an error"),
        None,
    );

    let installed = install(home.path(), &source, &VersionSpec::Latest, Channel::Release)
        .expect("a release published before sidecars must stay installable");
    assert_eq!(
        installed.verified, None,
        "an unverified install must report itself as one"
    );
    assert!(
        installed
            .root
            .join("bin")
            .join(executable_name("kira"))
            .is_file()
    );
}

#[test]
fn an_already_installed_version_verifies_nothing_because_it_fetches_nothing() {
    let releases = TempTree::create("again_feed");
    let home = TempTree::create("again_home");
    publish(
        releases.path(),
        Channel::Release,
        "1.10.0",
        &FixtureToolchain::complete(),
    );

    let source = DirectoryReleaseSource::new(releases.path()).expect("supported host");
    let first = install(home.path(), &source, &VersionSpec::Latest, Channel::Release)
        .expect("the first install");
    assert!(first.verified.is_some());

    let again = install(home.path(), &source, &VersionSpec::Latest, Channel::Release)
        .expect("the second install");
    assert!(again.already_installed);
    assert_eq!(
        again.verified, None,
        "nothing was fetched, so nothing was verified"
    );
}
