//! `kira_app_runner`: the runner an exported application carries inside it.
//!
//! An exported Xcode app links this crate as a static archive and reaches it
//! through one C entry point, [`kira_live_runner_entry`]. Its generated
//! `main.m` finds the app's bundled `KiraRunner.toml` and hands the path here;
//! everything after that is hosting a bundle, exactly as the desktop runner
//! does, through the same [`kira_bundle_host`] machinery:
//!
//! - `mode = "live"` connects to the live server the manifest names and runs
//!   what that server serves, taking reloads for as long as the app is up.
//! - `mode = "standalone"` plays the bundles embedded in the app's own
//!   resources and returns when the program returns.
//!
//! The archive is linked with `-Wl,-force_load`, and the app link carries
//! `-Wl,-export_dynamic`: a self-hosted native half binds its trampolines out
//! of the running image, so every symbol the runtime and the program need
//! must survive into the executable's export table.
//!
//! This crate compiles for every Apple target the toolchain cross-builds; it
//! deliberately has no LLVM dependency, because running a bundle never needs
//! one.

use std::ffi::{CStr, c_char, c_int};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use kira_bundle_host::{BundleHost, relay};
use kira_live::{ClientError, RunnerClient, RunnerHost};
use kira_manifest::platform_config::{RunnerKind, RunnerManifest, RuntimeMode};

/// Entry point the exported application's `main.m` calls.
///
/// `manifest_path` is the absolute path of the app's bundled `KiraRunner.toml`
/// (or any path the process can read). It returns when the hosted program
/// returns: 0 for a program that ran to an ordinary end, non-zero when the
/// bundle could not be read, connected, or started. The reason for a failure
/// is on stderr, prefixed `kira-app-runner:` — the same discipline the other
/// runners keep.
///
/// # Safety
/// `manifest_path` must be null or a valid NUL-terminated UTF-8 C string for
/// the duration of the call. The entry runs the program on the calling thread,
/// which on Apple platforms must be the process main thread.
#[unsafe(no_mangle)]
pub extern "C" fn kira_live_runner_entry(manifest_path: *const c_char) -> c_int {
    kira_native_bridge::retain_process_exports();

    let Some(manifest_path) = read_manifest_path(manifest_path) else {
        report("the application passed no runner manifest path");
        return EXIT_FAILURE;
    };

    match run(&manifest_path) {
        Ok(code) => code,
        Err(error) => {
            report(&error.to_string());
            EXIT_FAILURE
        }
    }
}

/// Exit code for an application that ran.
const EXIT_OK: c_int = 0;
/// Exit code for anything that did not.
const EXIT_FAILURE: c_int = 1;

/// Reads the manifest path out of the C string, `None` for null.
fn read_manifest_path(pointer: *const c_char) -> Option<PathBuf> {
    if pointer.is_null() {
        return None;
    }
    // SAFETY: the contract above — a valid NUL-terminated string, alive for
    // the call.
    let text = unsafe { CStr::from_ptr(pointer) };
    Some(PathBuf::from(text.to_string_lossy().into_owned()))
}

/// Why an embedded session could not run.
#[derive(Debug, thiserror::Error)]
enum RunError {
    /// The manifest could not be read or parsed.
    #[error("cannot read the runner manifest `{path}`: {source}")]
    Manifest {
        /// Where the manifest was expected.
        path: PathBuf,
        /// What was wrong with it.
        source: kira_manifest::platform_config::RunnerManifestError,
    },
    #[error(transparent)]
    Io(#[from] std::io::Error),
    /// The live session failed.
    #[error("{0}")]
    Session(#[from] ClientError),
    /// The protocol thread could not be started.
    #[error("could not start the live protocol thread: {0}")]
    Thread(#[source] std::io::Error),
    /// The protocol thread panicked while the app was still coming up.
    #[error("the live protocol thread panicked")]
    ProtocolPanicked,
    /// A standalone bundle could not be hosted.
    #[error("{0}")]
    Host(String),
}

/// Runs what the manifest at `manifest_path` describes, returning its exit
/// code.
fn run(manifest_path: &Path) -> Result<c_int, RunError> {
    let text = std::fs::read_to_string(manifest_path)?;
    let manifest = RunnerManifest::parse(&text).map_err(|source| RunError::Manifest {
        path: manifest_path.to_path_buf(),
        source,
    })?;
    let base = manifest_path.parent().unwrap_or(Path::new("."));

    match manifest.mode {
        RuntimeMode::Standalone => run_standalone(base, &manifest),
        RuntimeMode::Live => run_live(base, &manifest),
    }
}

/// Plays the bundles embedded in the app, on the calling thread.
///
/// There is nothing to stay up for afterwards: the program is entirely inside
/// this app, so its return is the return.
fn run_standalone(base: &Path, manifest: &RunnerManifest) -> Result<c_int, RunError> {
    let bundles_dir =
        embedded_bundles_dir(base, manifest).ok_or_else(|| missing_embedded_bundles(manifest))?;
    let bundle_dir = bundles_dir.join(format!("{}.klbundle", manifest.main_bundle_id));
    let bundle =
        kira_live::Bundle::read(&bundle_dir).map_err(|error| RunError::Host(error.to_string()))?;

    let mut host = BundleHost::new(cache_root(base, &manifest.local_cache_path));
    host.load(&bundle)
        .map_err(|error| RunError::Host(error.to_string()))?;
    host.link()
        .map_err(|error| RunError::Host(error.to_string()))?;
    // Start runs the program to its own end; the outcome says whether that end
    // was an ordinary return.
    host.start()
        .map_err(|error| RunError::Host(error.to_string()))?;
    Ok(EXIT_OK)
}

/// Connects to the live server and hosts the app for as long as it runs.
///
/// The split mirrors every Kira runner: the calling thread — the process main
/// thread, which an Apple app's event loop requires — owns every host call,
/// and a protocol thread owns the socket. The call returns when the app's run
/// loop ends or the server shuts the session down.
fn run_live(base: &Path, manifest: &RunnerManifest) -> Result<c_int, RunError> {
    let address = server_address(manifest)?;
    let client = RunnerClient::connect(address, manifest.kind.runner_id())?;

    let mut host = BundleHost::new(cache_root(base, &manifest.local_cache_path));
    let (relay, app) = relay::pair_with_hotpatch(host.hotpatch_disabled(), host.hotpatch_status());

    let protocol = std::thread::Builder::new()
        .name("kira-live-protocol".to_owned())
        .spawn(move || -> Result<(), ClientError> {
            let mut client = client;
            let mut relay = relay;
            client.run_session(&mut relay)?;
            client.serve_reloads(&mut relay)?;
            client.goodbye()
        })
        .map_err(RunError::Thread)?;

    app.serve(&mut host);

    protocol.join().map_err(|_| RunError::ProtocolPanicked)??;
    Ok(EXIT_OK)
}

/// The directory holding `<bundle-id>.klbundle` directories.
fn embedded_bundles_dir(base: &Path, manifest: &RunnerManifest) -> Option<PathBuf> {
    manifest
        .embedded_bundles_path
        .as_deref()
        .map(|relative| resolve_against(base, relative))
}

fn missing_embedded_bundles(manifest: &RunnerManifest) -> RunError {
    RunError::Host(format!(
        "the manifest names no embedded bundles directory, so standalone \
         playback cannot find bundle {}",
        manifest.main_bundle_id
    ))
}

/// Resolves the `[server]` section to a socket address.
fn server_address(manifest: &RunnerManifest) -> Result<SocketAddr, RunError> {
    let spelled = format!("{}:{}", manifest.server_host, manifest.server_port);
    // An exporter writes an IP, which parses directly; a developer may have
    // hand-edited a name in, which resolves.
    if let Ok(address) = spelled.parse() {
        return Ok(address);
    }
    use std::net::ToSocketAddrs as _;
    spelled
        .to_socket_addrs()?
        .next()
        .ok_or_else(|| RunError::Host(format!("`{spelled}` resolves to no address")))
}

/// Resolves the `[paths] local_cache` setting to a directory, creating it.
///
/// The spellings are the ones exporters write: an absolute path is taken as
/// given, `app-support/…` lands in the platform's application-support tree,
/// `app-cache/…` in its caches, `tmp/…` in the temporary directory, and
/// anything else is relative to the manifest itself — an app's sandbox decides
/// where those trees really are, so they are reached through `$HOME` rather
/// than assumed.
fn cache_root(base: &Path, local_cache: &str) -> PathBuf {
    let root = if Path::new(local_cache).is_absolute() {
        PathBuf::from(local_cache)
    } else if let Some(rest) = local_cache.strip_prefix("app-support/") {
        home().join("Library/Application Support").join(rest)
    } else if let Some(rest) = local_cache.strip_prefix("app-cache/") {
        home().join("Library/Caches").join(rest)
    } else if let Some(rest) = local_cache.strip_prefix("tmp/") {
        std::env::temp_dir().join(rest)
    } else {
        resolve_against(base, local_cache)
    };
    let _ = std::fs::create_dir_all(&root);
    root
}

/// The container root a relative well-known prefix resolves against.
fn home() -> PathBuf {
    std::env::var_os("KIRA_LIVE_APP_CONTAINER_ROOT")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(PathBuf::from))
        .unwrap_or_else(std::env::temp_dir)
}

fn resolve_against(base: &Path, relative: &str) -> PathBuf {
    let path = Path::new(relative);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        base.join(path)
    }
}

/// Reports a failure the way every Kira runner does.
fn report(message: &str) {
    eprintln!("kira-app-runner: {message}");
}

/// Whether `kind` is an Xcode-built application runner.
///
/// Exposed for the generator side's tests: a desktop runner manifest is a
/// desktop concern, and an exported app never ships one today, but the model
/// accepts the value and the runner would host it — so the mapping is total
/// rather than assumed partial.
#[allow(dead_code)]
fn hosts(kind: RunnerKind) -> bool {
    matches!(
        kind,
        RunnerKind::Desktop
            | RunnerKind::XcodeMacos
            | RunnerKind::XcodeIos
            | RunnerKind::XcodeTvos
            | RunnerKind::XcodeVisionos
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(local_cache: &str, embedded: Option<&str>) -> RunnerManifest {
        RunnerManifest {
            kind: RunnerKind::XcodeMacos,
            name: "KiraApp".to_owned(),
            bundle_id: "com.kira.live.dev".to_owned(),
            version: "0.1.0".to_owned(),
            mode: RuntimeMode::Standalone,
            target_path: "/projects/demo".to_owned(),
            package_name: "demo".to_owned(),
            local_cache_path: local_cache.to_owned(),
            main_bundle_id: "com.kira.demo".to_owned(),
            embedded_bundles_path: embedded.map(str::to_owned),
            server_host: "127.0.0.1".to_owned(),
            server_port: 0,
            native_contract_hash: String::new(),
        }
    }

    #[test]
    fn well_known_cache_prefixes_resolve_into_the_container_tree() {
        let base = Path::new("/Applications/KiraApp.app/Contents/Resources");
        // SAFETY: single-threaded test process; no other thread reads the
        // environment while this var is replaced and restored.
        unsafe { std::env::set_var("KIRA_LIVE_APP_CONTAINER_ROOT", "/containers/app") };

        assert_eq!(
            cache_root(base, "app-support/KiraExport"),
            PathBuf::from("/containers/app/Library/Application Support/KiraExport")
        );
        assert_eq!(
            cache_root(base, "app-cache/KiraExport"),
            PathBuf::from("/containers/app/Library/Caches/KiraExport")
        );
        assert_eq!(
            cache_root(base, "tmp/KiraExport"),
            std::env::temp_dir().join("KiraExport")
        );
        assert_eq!(
            cache_root(base, "cache"),
            base.join("cache"),
            "an unknown prefix is the manifest's own concern"
        );
        // SAFETY: as above — the test owns the process for this variable.
        unsafe { std::env::remove_var("KIRA_LIVE_APP_CONTAINER_ROOT") };
    }

    #[test]
    fn embedded_bundles_resolve_relative_to_the_manifest() {
        let base = Path::new("/Applications/KiraApp.app/Contents/Resources");
        assert_eq!(
            embedded_bundles_dir(base, &manifest("", Some("Bundles"))),
            Some(PathBuf::from(
                "/Applications/KiraApp.app/Contents/Resources/Bundles"
            ))
        );
        assert_eq!(embedded_bundles_dir(base, &manifest("", None)), None);
    }

    #[test]
    fn a_server_spelling_parses_and_a_host_resolves_to_something() {
        let mut manifest = manifest("", None);
        manifest.server_host = "127.0.0.1".to_owned();
        manifest.server_port = 42_111;
        assert_eq!(
            server_address(&manifest).expect("parses"),
            "127.0.0.1:42111".parse().unwrap()
        );

        manifest.server_host = "localhost".to_owned();
        assert!(server_address(&manifest).is_ok(), "a name resolves");
    }

    #[test]
    fn every_runner_kind_this_crate_ships_is_hosted() {
        for kind in [
            RunnerKind::Desktop,
            RunnerKind::XcodeMacos,
            RunnerKind::XcodeIos,
            RunnerKind::XcodeTvos,
            RunnerKind::XcodeVisionos,
        ] {
            assert!(hosts(kind));
        }
    }
}
