//! The fixture release feed every knvm integration test is driven against.
//!
//! Nothing here touches the developer's real `~/.kira` and nothing here opens a
//! network connection. A fixture release is a real `.tar.gz` built by the system
//! `tar`, so extraction, validation, and the move into place are genuinely
//! exercised by whichever test consumes it — only the transport is substituted.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use kira_knvm::{Channel, archive_file_name};

/// A temp directory that removes itself when the test ends, pass or fail.
pub struct TempTree {
    path: PathBuf,
}

impl TempTree {
    /// Creates a uniquely-named temp directory, following the repo's
    /// `{pid}_{unique}` pattern so parallel tests never collide.
    pub fn create(label: &str) -> Self {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        let pid = std::process::id();
        let path = std::env::temp_dir().join(format!("knvm_test_{label}_{pid}_{unique}"));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("create temp tree");
        Self { path }
    }

    /// The directory itself.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempTree {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// The host key every fixture archive is published under.
///
/// The tests name the running host so `DirectoryReleaseSource::new` — the same
/// constructor the binary uses — finds them.
pub fn host_key() -> &'static str {
    kira_knvm::current_host_key().expect("the test host must be a supported Kira host")
}

/// What a fixture toolchain ships. Omitting a file is how the validation guards
/// build a deliberately broken release.
pub struct FixtureToolchain {
    /// Whether `bin/kirac` is present and executable.
    pub with_primary_binary: bool,
    /// Whether `foundation/package.kira` is present.
    pub with_foundation: bool,
}

impl FixtureToolchain {
    /// A complete, valid toolchain.
    pub fn complete() -> Self {
        Self {
            with_primary_binary: true,
            with_foundation: true,
        }
    }
}

/// Builds `<releases>/<channel>/<version>/kira-<version>-<host>.tar.gz` holding
/// a toolchain tree.
///
/// `bin/kirac` is an executable shell script that prints a marker naming its own
/// version, so a dispatch test can prove *which* installed toolchain ran rather
/// than that something ran.
pub fn publish(releases: &Path, channel: Channel, version: &str, shape: &FixtureToolchain) {
    let staging = releases.join(".build").join(version);
    let _ = std::fs::remove_dir_all(&staging);
    let payload = staging.join(format!("kira-{version}"));

    if shape.with_primary_binary {
        let bin = payload.join("bin");
        std::fs::create_dir_all(&bin).expect("create bin");
        let script = bin.join("kirac");
        std::fs::write(
            &script,
            format!("#!/bin/sh\necho \"kirac {version} argv: $*\"\nexit 0\n"),
        )
        .expect("write fixture kirac");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755))
                .expect("mark fixture kirac executable");
        }
    } else {
        // A release with no binary at all still has a tree, so the failure under
        // test is the validation refusal and not an unpack failure.
        std::fs::create_dir_all(payload.join("bin")).expect("create bin");
    }

    if shape.with_foundation {
        let foundation = payload.join("foundation");
        std::fs::create_dir_all(foundation.join("app")).expect("create foundation");
        std::fs::write(
            foundation.join("package.kira"),
            "Package Foundation {\n    let version = \"0.1.0\"\n}\n",
        )
        .expect("write foundation manifest");
        std::fs::write(
            foundation.join("app").join("foundation.kira"),
            "function foundationVersion() -> String { return \"fixture\" }\n",
        )
        .expect("write foundation source");
    }

    for template in ["app", "library"] {
        std::fs::create_dir_all(payload.join("templates").join(template))
            .expect("create template dir");
    }
    std::fs::create_dir_all(payload.join("packages").join("kira_main"))
        .expect("create packages dir");

    let version_dir = releases.join(channel.dir_name()).join(version);
    std::fs::create_dir_all(&version_dir).expect("create release version dir");
    let archive = version_dir.join(archive_file_name(version, host_key()));

    let status = std::process::Command::new("tar")
        .arg("-czf")
        .arg(&archive)
        .arg("-C")
        .arg(&staging)
        .arg(format!("kira-{version}"))
        .status()
        .expect("run tar to build the fixture archive");
    assert!(status.success(), "fixture archive must be built by tar");

    std::fs::remove_dir_all(&staging).expect("clean fixture staging");
}
