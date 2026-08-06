//! `kira-desktop-runner`: the desktop runner client binary.
//!
//! A live server starts this and hands it an address; it connects, downloads the
//! bundle, runs it, and reports each milestone it actually reaches.
//!
//! ```text
//! kira-desktop-runner --server 127.0.0.1:4000 [--cache <dir>]
//! ```
//!
//! It exits 0 only when the session reached its entrypoint. Any other outcome is
//! a non-zero exit with the reason on stderr — a runner that could not run the
//! app must not look like one that did.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use kira_desktop_runner::DesktopHost;
use kira_desktop_runner::relay::{self, RelayHost};
use kira_live::{ClientError, RunnerClient};
use kira_manifest::RunnerId;

/// Exit code for a session that ran.
const EXIT_OK: u8 = 0;
/// Exit code for a session that did not.
const EXIT_FAILURE: u8 = 1;
/// Exit code for bad usage.
const EXIT_USAGE: u8 = 2;

fn main() -> ExitCode {
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
}

/// Connects, hosts the app, takes reloads, and says goodbye.
///
/// The app gets this thread and the protocol gets another. That split is what
/// lets a runner host an app rather than only a program: an app's run loop owns
/// the thread that starts it for as long as the window is open, and on macOS
/// that thread has to be this one.
fn run(options: &Options) -> Result<(), RunError> {
    let client = RunnerClient::connect(options.server, RunnerId::Desktop)?;
    let mut host = DesktopHost::new(options.cache.clone());
    let (relay, app) = relay::pair(host.hotpatch_disabled());
    let running = app.running();

    let protocol = std::thread::Builder::new()
        .name("kira-live-protocol".to_owned())
        .spawn(move || serve_session(client, relay, &running))
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
fn serve_session(
    mut client: RunnerClient,
    mut relay: RelayHost,
    running: &Arc<AtomicBool>,
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
        std::process::exit(i32::from(code));
    }
    outcome
}

/// One session: bring the app up, then stay up for it.
fn session(client: &mut RunnerClient, relay: &mut RelayHost) -> Result<(), ClientError> {
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
    /// Where to stage the bundle.
    cache: PathBuf,
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
            cache: cache.unwrap_or_else(default_cache),
        })
    }
}

/// Where a runner stages bundles when it is not told.
fn default_cache() -> PathBuf {
    std::env::temp_dir().join(format!("kira-live-runner-{}", std::process::id()))
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
        assert_eq!(options.cache, PathBuf::from("/tmp/x"));
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
