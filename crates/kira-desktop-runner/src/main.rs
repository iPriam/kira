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

use kira_desktop_runner::DesktopHost;
use kira_live::RunnerClient;
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

/// Connects, runs the session, and says goodbye.
fn run(options: &Options) -> Result<(), kira_live::ClientError> {
    let mut client = RunnerClient::connect(options.server, RunnerId::Desktop)?;
    let mut host = DesktopHost::new(options.cache.clone());
    client.run_session(&mut host)?;
    // The entrypoint returned, so this runner has done everything a headless
    // desktop runner can. A windowed runner would keep going and report frames.
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
