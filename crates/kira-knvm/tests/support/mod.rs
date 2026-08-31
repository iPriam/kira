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
    /// Whether `bin/kira` is present and executable.
    pub with_primary_binary: bool,
    /// Whether `bin/kira-language-server` is present and executable.
    pub with_language_server: bool,
    /// Whether the host compiler bridge archive is present.
    pub with_compiler_bridge: bool,
    /// Whether `foundation/package.kira` is present.
    pub with_foundation: bool,
}

impl FixtureToolchain {
    /// A complete, valid toolchain.
    pub fn complete() -> Self {
        Self {
            with_primary_binary: true,
            with_language_server: true,
            with_compiler_bridge: true,
            with_foundation: true,
        }
    }
}

/// Builds `<releases>/<channel>/<version>/kira-<version>-<host>.tar.gz` holding
/// a toolchain tree.
///
/// `bin/kira` is an executable shell script that prints a marker naming its own
/// version, so a dispatch test can prove *which* installed toolchain ran rather
/// than that something ran.
/// A static runtime archive.s file name on the host this test runs on.
///
/// Distinct from `kira_knvm::archive_file_name`, which names a release
/// tarball.
///
/// MSVC names one `<name>.lib`; everything else `lib<name>.a`. The installer
/// looks for the host's spelling, so a fixture that only ever wrote the Unix
/// one is refused on Windows for a file it did ship.
fn runtime_archive_name(name: &str) -> String {
    match cfg!(target_env = "msvc") {
        true => format!("{name}.lib"),
        false => format!("lib{name}.a"),
    }
}

/// Writes a fixture tool that prints `<name> <version> argv: <args>` when run.
///
/// A shell script serves on Unix. Windows runs no such thing: an installer
/// looks for `<name>.exe`, and a `#!/bin/sh` file under that name is not a
/// program, so a dispatch test sees empty output rather than a marker. So the
/// Windows build compiles a real executable, once per process, and stamps the
/// marker in through `rustc --cfg`-free means: the marker is read from a file
/// beside the binary, so one compiled helper serves every version.
fn write_fixture_tool(bin: &Path, name: &str, version: &str) {
    let path = bin.join(format!("{name}{}", std::env::consts::EXE_SUFFIX));
    if cfg!(windows) {
        std::fs::write(
            bin.join(format!("{name}.marker")),
            format!("{name} {version}"),
        )
        .expect("write fixture marker");
        std::fs::copy(fixture_helper(), &path).expect("copy fixture tool");
        return;
    }
    std::fs::write(
        &path,
        format!("#!/bin/sh\necho \"{name} {version} argv: $*\"\nexit 0\n"),
    )
    .expect("write fixture tool");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
            .expect("mark fixture tool executable");
    }
}

/// The compiled helper every Windows fixture tool is a copy of.
///
/// Built once per test process: `rustc` is not cheap enough to run per publish,
/// and the marker file beside each copy is what makes one binary answer for
/// every tool and version.
#[cfg(windows)]
fn fixture_helper() -> PathBuf {
    use std::sync::OnceLock;
    static HELPER: OnceLock<PathBuf> = OnceLock::new();
    HELPER
        .get_or_init(|| {
            let dir =
                std::env::temp_dir().join(format!("knvm_fixture_tool_{}", std::process::id()));
            std::fs::create_dir_all(&dir).expect("fixture tool dir");
            let source = dir.join("tool.rs");
            std::fs::write(
                &source,
                r#"fn main() {
    let exe = std::env::current_exe().expect("own path");
    let marker = exe.with_extension("marker");
    let text = std::fs::read_to_string(&marker).unwrap_or_default();
    let args: Vec<String> = std::env::args().skip(1).collect();
    println!("{} argv: {}", text.trim(), args.join(" "));
}
"#,
            )
            .expect("write fixture tool source");
            let out = dir.join("tool.exe");
            let status = std::process::Command::new("rustc")
                .args(["-O", "--edition", "2021"])
                .arg(&source)
                .arg("-o")
                .arg(&out)
                .status()
                .expect("run rustc for the fixture tool");
            assert!(status.success(), "the fixture tool must compile");
            out
        })
        .clone()
}

#[cfg(not(windows))]
fn fixture_helper() -> PathBuf {
    unreachable!("only the Windows path copies a compiled helper")
}

pub fn publish(releases: &Path, channel: Channel, version: &str, shape: &FixtureToolchain) {
    let staging = releases.join(".build").join(version);
    let _ = std::fs::remove_dir_all(&staging);
    let payload = staging.join(format!("kira-{version}"));

    if shape.with_primary_binary {
        let bin = payload.join("bin");
        std::fs::create_dir_all(&bin).expect("create bin");
        write_fixture_tool(&bin, "kira", version);
    } else {
        // A release with no binary at all still has a tree, so the failure under
        // test is the validation refusal and not an unpack failure.
        std::fs::create_dir_all(payload.join("bin")).expect("create bin");
    }

    if shape.with_language_server {
        let bin = payload.join("bin");
        std::fs::create_dir_all(&bin).expect("create bin");
        let server = bin.join(format!(
            "kira-language-server{}",
            std::env::consts::EXE_SUFFIX
        ));
        std::fs::write(
            &server,
            format!("#!/bin/sh\necho \"kira-language-server {version} argv: $*\"\nexit 0\n"),
        )
        .expect("write fixture language server");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&server, std::fs::Permissions::from_mode(0o755))
                .expect("mark fixture language server executable");
        }
    }

    let bin = payload.join("bin");
    std::fs::create_dir_all(&bin).expect("create bin for runtime archives");
    for archive in [
        runtime_archive_name("kira_native_bridge"),
        // The wasm archive is built for emscripten whatever the host is, so it
        // keeps the `lib….a` spelling everywhere.
        "libkira_native_bridge-wasm32-emscripten.a".to_owned(),
    ] {
        std::fs::write(bin.join(archive), "fixture runtime archive")
            .expect("write fixture runtime archive");
    }
    if shape.with_compiler_bridge {
        std::fs::write(
            bin.join(runtime_archive_name("kira_compiler_bridge")),
            "fixture compiler runtime archive",
        )
        .expect("write fixture compiler runtime archive");
    }
    #[cfg(target_os = "macos")]
    for target in [
        format!("{}-apple-darwin", std::env::consts::ARCH),
        "aarch64-apple-ios".to_owned(),
        "aarch64-apple-ios-sim".to_owned(),
        "aarch64-apple-tvos".to_owned(),
        "aarch64-apple-tvos-sim".to_owned(),
        "aarch64-apple-visionos".to_owned(),
        "aarch64-apple-visionos-sim".to_owned(),
    ] {
        let archive_dir = bin.join(target);
        std::fs::create_dir_all(&archive_dir).expect("create Apple runner directory");
        std::fs::write(
            archive_dir.join("libkira_app_runner.a"),
            "fixture Apple runner archive",
        )
        .expect("write fixture Apple runner archive");
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

    // A published release carries its digest beside it, so the default fixture
    // exercises the verifying path rather than the unverified fallback. A test
    // that wants the other case removes or rewrites this file.
    let digest = kira_knvm::Sha256::of_file(&archive).expect("hash the fixture archive");
    std::fs::write(
        checksum_sidecar_path(releases, channel, version),
        format!("{digest}  {}\n", archive_file_name(version, host_key())),
    )
    .expect("write the fixture checksum sidecar");

    std::fs::remove_dir_all(&staging).expect("clean fixture staging");
}

/// Where [`publish`] writes a release's checksum sidecar.
///
/// Exposed so a test can delete it (a release published before sidecars) or
/// rewrite it (a corrupted or substituted download).
pub fn checksum_sidecar_path(releases: &Path, channel: Channel, version: &str) -> PathBuf {
    releases
        .join(channel.dir_name())
        .join(version)
        .join(kira_knvm::checksum_file_name(&archive_file_name(
            version,
            host_key(),
        )))
}
