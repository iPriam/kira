//! The desktop runner: the client binary that hosts a Kira app in a live
//! session.
//!
//! ```text
//! kira-desktop-runner --server 127.0.0.1:4000 [--cache <dir>]
//! ```
//!
//! Everything it does to actually run a bundle — staging, loading, linking,
//! swapping — is [`kira-bundle-host`], shared with every other Kira runner.
//! What this binary adds is only its shape: process entry, argument parsing,
//! a scratch cache of its own, and the rule that it exits 0 only when the
//! session reached its entrypoint. Any other outcome is a non-zero exit with
//! the reason on stderr — a runner that could not run the app must not look
//! like one that did.
//!
//! The `lib` target exists so a workspace build of `kira-cli` also builds this
//! binary: cargo never builds a dependency's `[[bin]]`, but it always builds
//! its lib, and the CLI's live verb needs the runner sitting beside it. The
//! lib itself is a re-export face, not a second implementation.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use kira_bundle_host::{BundleHost, relay};
use kira_live::{ClientError, RunnerClient};
use kira_manifest::RunnerId;

/// Exit code for a session that ran.
const EXIT_OK: u8 = 0;
/// Exit code for a session that did not.
const EXIT_FAILURE: u8 = 1;
/// Exit code for bad usage.
const EXIT_USAGE: u8 = 2;

fn main() -> ExitCode {
    kira_native_bridge::retain_process_exports();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let options = match Options::parse(&args) {
        Ok(options) => options,
        Err(error) => {
            eprintln!("kira-desktop-runner: {error}");
            eprintln!("usage: kira-desktop-runner --server <addr> [--cache <dir>]");
            return ExitCode::from(EXIT_USAGE);
        }
    };

    match run(&options) {
        Ok(()) => ExitCode::from(EXIT_OK),
        Err(error) => {
            eprintln!("kira-desktop-runner: {error}");
            ExitCode::from(EXIT_FAILURE)
        }
    }
}

/// Why the runner stopped.
#[derive(Debug, thiserror::Error)]
enum RunError {
    /// The session itself failed.
    #[error("{0}")]
    Session(#[from] ClientError),
    /// The protocol thread could not be started.
    #[error("could not start the live protocol thread: {0}")]
    Thread(#[source] std::io::Error),
    /// The protocol thread panicked.
    #[error("the live protocol thread panicked")]
    ProtocolPanicked,
    /// The scratch cache directory could not be created.
    #[error("could not create the runner's staging directory: {0}")]
    Scratch(#[source] std::io::Error),
    /// Every scratch name this process could take was already taken.
    #[error(
        "every staging directory name for this process is taken; \
         remove the `kira-live-runner-*` directories in the system temporary directory"
    )]
    ScratchExhausted,
}

/// Connects, hosts the app, takes reloads, and says goodbye.
///
/// The app gets this thread and the protocol gets another. That split is what
/// lets a runner host an app rather than only a program: an app's run loop owns
/// the thread that starts it for as long as the window is open, and on macOS
/// that thread has to be this one.
fn run(options: &Options) -> Result<(), RunError> {
    let cache = Arc::new(RunnerCache::open(options.cache.clone())?);
    let client = RunnerClient::connect(options.server, RunnerId::Desktop)?;
    let mut host = BundleHost::new(cache.path.clone());
    let (relay, app) = relay::pair_with_hotpatch(host.hotpatch_disabled(), host.hotpatch_status());
    let running = app.running();

    let session_cache = Arc::clone(&cache);
    let protocol = std::thread::Builder::new()
        .name("kira-live-protocol".to_owned())
        .spawn(move || serve_session(client, relay, &running, &session_cache))
        .map_err(RunError::Thread)?;

    // Every host call happens here, in order, on the one thread that is allowed
    // to run the app — the protocol thread only ever asks. It returns when the
    // session is over, which for an app is after its window has closed.
    app.serve(&mut host);

    protocol.join().map_err(|_| RunError::ProtocolPanicked)??;
    Ok(())
}

/// The protocol half of the session, on its own thread.
///
/// Ends the process itself when the app is still running, because then nothing
/// else can: the main thread is inside a run loop that returns when the window
/// closes, and the session has just been told to shut down.
///
/// The exit path releases the staging cache first. Skipping unwinding would
/// otherwise strand every staged bundle in the system temporary directory — one
/// per Ctrl-C'd session, reclaimed by nobody.
fn serve_session(
    mut client: RunnerClient,
    mut relay: relay::RelayHost,
    running: &Arc<AtomicBool>,
    cache: &RunnerCache,
) -> Result<(), ClientError> {
    let outcome = session(&mut client, &mut relay);
    if running.load(Ordering::SeqCst) {
        let code = match &outcome {
            Ok(()) => EXIT_OK,
            Err(error) => {
                eprintln!("kira-desktop-runner: {error}");
                EXIT_FAILURE
            }
        };
        cache.release();
        std::process::exit(i32::from(code));
    }
    outcome
}

/// One session: bring the app up, then stay up for it.
fn session(client: &mut RunnerClient, relay: &mut relay::RelayHost) -> Result<(), ClientError> {
    client.run_session(relay)?;

    // The app is running and the runner stays up. This is what makes a reload
    // possible at all: the process, its cache, and its loaded native library are
    // still here, waiting to be handed new code.
    //
    // It is also where this runner's swap point is. The specification applies a
    // swap "at a frame boundary when the VM is idle", and idle is the word that
    // does the work: a swap is offered to the host, and a host whose entrypoint
    // is still running refuses it rather than pulling a module out from under a
    // live call stack. The session relaunches instead, and says so.
    client.serve_reloads(relay)?;
    client.goodbye()?;
    Ok(())
}

/// What this runner was told to do.
struct Options {
    /// The live server to connect to.
    server: SocketAddr,
    /// Where to stage the bundle, or `None` to use a scratch directory of the
    /// runner's own.
    cache: Option<PathBuf>,
}

/// A usage error.
#[derive(Debug, thiserror::Error)]
enum OptionsError {
    #[error("expected `--server <addr>`")]
    NoServer,
    #[error("`{0}` needs a value")]
    MissingValue(String),
    #[error("`{value}` is not an address the runner can connect to: {source}")]
    BadAddress {
        value: String,
        #[source]
        source: std::net::AddrParseError,
    },
    #[error("unexpected argument `{0}`")]
    Unexpected(String),
}

impl Options {
    /// Parses the runner's arguments.
    fn parse(args: &[String]) -> Result<Options, OptionsError> {
        let mut server = None;
        let mut cache = None;

        let mut index = 0;
        while index < args.len() {
            let flag = args[index].as_str();
            match flag {
                "--server" | "--cache" => {
                    let value = args
                        .get(index + 1)
                        .ok_or_else(|| OptionsError::MissingValue(flag.to_owned()))?;
                    if flag == "--server" {
                        server =
                            Some(value.parse().map_err(|source| OptionsError::BadAddress {
                                value: value.clone(),
                                source,
                            })?);
                    } else {
                        cache = Some(PathBuf::from(value));
                    }
                    index += 2;
                }
                other => return Err(OptionsError::Unexpected(other.to_owned())),
            }
        }

        Ok(Options {
            server: server.ok_or(OptionsError::NoServer)?,
            cache,
        })
    }
}

/// Where this runner stages bundles.
///
/// A runner told `--cache <dir>` stages there and leaves it alone; one that was
/// not gets a directory of its own under the system temporary directory and
/// removes it when the session ends normally.
struct RunnerCache {
    path: PathBuf,
    owned: bool,
}

/// How many names a runner tries before giving up on a scratch directory.
///
/// Only leftovers of *this* process id are in the way, and only a runner that
/// was killed leaves one, so the first name is free in every ordinary run.
const SCRATCH_ATTEMPTS: u32 = 4096;

impl RunnerCache {
    /// Opens the cache the options asked for, creating a scratch directory when
    /// none was named.
    ///
    /// The scratch directory is created exclusively rather than named and
    /// assumed free: process ids are reused, and a directory a killed runner
    /// left behind holds files but no bundle manifest, which staging refuses to
    /// clear. Creating the directory is what makes the name this run's.
    fn open(named: Option<PathBuf>) -> Result<Self, RunError> {
        if let Some(path) = named {
            return Ok(Self { path, owned: false });
        }
        let base = std::env::temp_dir();
        let pid = std::process::id();
        for attempt in 0..SCRATCH_ATTEMPTS {
            let path = base.join(format!("kira-live-runner-{pid}-{attempt}"));
            match std::fs::create_dir(&path) {
                Ok(()) => return Ok(Self { path, owned: true }),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(RunError::Scratch(error)),
            }
        }
        Err(RunError::ScratchExhausted)
    }

    /// Removes the scratch this cache owns, if it has not gone already.
    ///
    /// Idempotent, and safe to run before [`std::process::exit`]: dropping
    /// without unwinding never runs `Drop`, so an exit that ends the process
    /// must release what it owns first.
    fn release(&self) {
        if self.owned {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }
}

impl Drop for RunnerCache {
    fn drop(&mut self) {
        self.release();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn parses_a_server_address() {
        let options = Options::parse(&args(&["--server", "127.0.0.1:4000"])).expect("parses");
        assert_eq!(options.server, "127.0.0.1:4000".parse().unwrap());
    }

    #[test]
    fn parses_a_cache_directory() {
        let options = Options::parse(&args(&["--server", "127.0.0.1:1", "--cache", "/tmp/x"]))
            .expect("parses");
        assert_eq!(options.cache, Some(PathBuf::from("/tmp/x")));
    }

    /// Two runners of the same process id take different scratch directories,
    /// which is what a reused process id needs and a bare name cannot give.
    #[test]
    fn two_scratch_caches_of_one_process_do_not_collide() {
        let first = RunnerCache::open(None).expect("first scratch");
        let second = RunnerCache::open(None).expect("second scratch");
        assert_ne!(first.path, second.path);
        assert!(first.path.is_dir());
        let path = first.path.clone();
        drop(first);
        assert!(!path.exists(), "a runner removes the scratch it owns");
    }

    /// A named cache is the caller's: the runner stages there and leaves it.
    #[test]
    fn a_named_cache_is_not_removed() {
        let named = std::env::temp_dir().join("kira-runner-named-cache-test");
        std::fs::create_dir_all(&named).expect("named cache");
        let cache = RunnerCache::open(Some(named.clone())).expect("named");
        assert_eq!(cache.path, named);
        drop(cache);
        assert!(named.is_dir());
        let _ = std::fs::remove_dir_all(&named);
    }

    #[test]
    fn a_runner_without_a_server_is_a_usage_error() {
        assert!(matches!(
            Options::parse(&args(&[])),
            Err(OptionsError::NoServer)
        ));
    }

    #[test]
    fn a_bad_address_is_a_usage_error() {
        assert!(matches!(
            Options::parse(&args(&["--server", "not-an-address"])),
            Err(OptionsError::BadAddress { .. })
        ));
    }

    #[test]
    fn a_flag_without_a_value_is_a_usage_error() {
        assert!(matches!(
            Options::parse(&args(&["--server"])),
            Err(OptionsError::MissingValue(_))
        ));
    }

    #[test]
    fn an_unknown_flag_is_a_usage_error() {
        assert!(matches!(
            Options::parse(&args(&["--wat"])),
            Err(OptionsError::Unexpected(_))
        ));
    }
}
