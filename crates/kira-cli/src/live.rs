//! `kirac live`: build a bundle, serve it, and run it on a runner.
//!
//! A live session is a server and a client, not a rebuild loop. This builds the
//! program into a `.klbundle`, binds a live server on the loopback, starts the
//! runner client as a real child process, and reports what the runner actually
//! reports back. The session is only ready when the runner says it got there.
//!
//! ```text
//! kirac live [runner] <file> [--backend vm|hybrid]
//! ```
//!
//! Every runner id parses, because a command that fails to parse cannot explain
//! itself. A runner this build cannot yet drive gets a precise diagnostic saying
//! so — never silence, and never a session that claims to have run somewhere it
//! did not.

use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::path::{Path, PathBuf};
use std::process::{Child, Command};

use kira_ir::IrProgram;
use kira_live::{
    Bundle, BundleError, LiveEvent, LiveServer, NamedPayload, PayloadKind, ServerError,
};
use kira_manifest::{BuildProfile, RunnerId};

use crate::hybrid;
use crate::native::Artifacts;

/// The backend a live session builds its bundle with.
///
/// A live bundle is either bytecode alone or a hybrid pair. There is no
/// LLVM-executable option: a live runner loads a bundle into its own process, so
/// the native half must be a library it can link, which is what hybrid is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LiveBackend {
    /// The VM half only: one bytecode payload.
    Vm,
    /// Both halves: a hybrid manifest, its bytecode, and its native library.
    Hybrid,
}

impl LiveBackend {
    /// The backend a `--backend` value names.
    fn parse(value: &str) -> Option<LiveBackend> {
        match value {
            "vm" => Some(Self::Vm),
            "hybrid" => Some(Self::Hybrid),
            _ => None,
        }
    }

    /// A label for diagnostics.
    fn label(self) -> &'static str {
        match self {
            Self::Vm => "vm",
            Self::Hybrid => "hybrid",
        }
    }
}

/// What a `kirac live` invocation asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveOptions {
    /// The runner to run on.
    pub runner: RunnerId,
    /// The program to run.
    pub path: String,
    /// Which backend builds the bundle.
    pub backend: LiveBackend,
}

/// A usage error in a `kirac live` invocation.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LiveOptionsError {
    /// No program was named.
    #[error("expected a path to a .kira file")]
    NoPath,
    /// A flag needed a value it did not get.
    #[error("`{0}` needs a value")]
    MissingValue(String),
    /// A `--backend` value named no live backend.
    #[error("unknown backend `{0}`; live sessions run `vm` or `hybrid`")]
    UnknownBackend(String),
    /// More than one program was named.
    #[error("expected one path to run, but got both `{first}` and `{second}`")]
    TooManyPaths {
        /// The first path.
        first: String,
        /// The second.
        second: String,
    },
}

impl LiveOptions {
    /// Parses `kirac live`'s arguments.
    ///
    /// The first positional is a runner if it names one and a path otherwise, so
    /// `kirac live ios` is the iOS runner while `kirac live ./ios` is a path.
    /// The distinction is made on shape, not on what happens to exist on disk:
    /// a path-looking argument stays a path even when nothing is there, so the
    /// error says the file is missing rather than that the runner is unknown.
    pub fn parse(args: &[String]) -> Result<LiveOptions, LiveOptionsError> {
        let mut runner = None;
        let mut path: Option<String> = None;
        let mut backend = LiveBackend::Vm;

        let mut index = 0;
        while index < args.len() {
            let argument = args[index].as_str();
            match argument {
                "--backend" => {
                    let value = args
                        .get(index + 1)
                        .ok_or_else(|| LiveOptionsError::MissingValue("--backend".to_owned()))?;
                    backend = LiveBackend::parse(value)
                        .ok_or_else(|| LiveOptionsError::UnknownBackend(value.clone()))?;
                    index += 2;
                }
                positional => {
                    // A runner id only in the first positional slot, and only
                    // when it does not look like a path.
                    if runner.is_none()
                        && path.is_none()
                        && !looks_like_path(positional)
                        && let Some(named) = RunnerId::parse(positional)
                    {
                        runner = Some(named);
                    } else if let Some(first) = &path {
                        return Err(LiveOptionsError::TooManyPaths {
                            first: first.clone(),
                            second: positional.to_owned(),
                        });
                    } else {
                        path = Some(positional.to_owned());
                    }
                    index += 1;
                }
            }
        }

        Ok(LiveOptions {
            // Desktop is the default runner: a live session with no runner named
            // is a session on the machine you are sitting at.
            runner: runner.unwrap_or(RunnerId::Desktop),
            path: path.ok_or(LiveOptionsError::NoPath)?,
            backend,
        })
    }
}

/// Whether `value` is shaped like a path rather than a bare runner id.
///
/// `kira live ios` means the iOS runner; `kira live ./ios` means the directory.
fn looks_like_path(value: &str) -> bool {
    value.contains('/') || value.contains('\\') || value.contains('.')
}

/// An error running a live session.
#[derive(Debug, thiserror::Error)]
pub enum LiveError {
    /// The runner is one this build cannot drive yet.
    ///
    /// Precise on purpose: the runner is modeled, the command parsed, and this
    /// says exactly what is missing rather than pretending the id is unknown.
    #[error(
        "the `{runner}` runner has no runner client in this build yet; \
         `desktop` is the runner that runs today"
    )]
    NoRunnerClient {
        /// The runner that was asked for.
        runner: &'static str,
    },
    /// The bundle could not be built.
    ///
    /// Carries the backend, because "the bundle would not build" is a different
    /// problem depending on which half failed to produce it.
    #[error("could not build the live bundle with the `{backend}` backend: {reason}")]
    Build {
        /// The backend that was building it.
        backend: &'static str,
        /// What went wrong.
        reason: String,
    },
    /// The bundle could not be assembled.
    #[error("could not assemble the live bundle: {0}")]
    Bundle(#[from] BundleError),
    /// The live server failed.
    #[error("live server failed: {0}")]
    Server(#[from] ServerError),
    /// The runner client binary could not be found or started.
    #[error("could not start the `{runner}` runner client at `{path}`: {source}")]
    Spawn {
        /// The runner it was for.
        runner: &'static str,
        /// Where the binary was looked for.
        path: PathBuf,
        /// The underlying failure.
        #[source]
        source: std::io::Error,
    },
    /// This process could not work out where it is, so it cannot find its runner.
    #[error("could not locate the runner client beside this executable: {0}")]
    Locate(#[source] std::io::Error),
    /// An i/o failure reading a built artifact.
    #[error("could not read the built artifact `{path}`: {source}")]
    Io {
        /// The artifact's path.
        path: PathBuf,
        /// The underlying failure.
        #[source]
        source: std::io::Error,
    },
}

impl LiveError {
    /// A build failure, naming the backend that hit it.
    fn build(backend: LiveBackend, error: &dyn std::fmt::Display) -> LiveError {
        LiveError::Build {
            backend: backend.label(),
            reason: error.to_string(),
        }
    }
}

/// A runner child process that is killed if the session unwinds.
///
/// A live session that fails must not leave an orphan runner holding the app's
/// window and the bundle's files. Killing on drop makes that structural.
struct RunnerProcess {
    child: Child,
}

impl Drop for RunnerProcess {
    fn drop(&mut self) {
        // Best effort: the process may already have exited, which is the normal
        // case and not worth reporting.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Builds `program` into a live bundle for `runner`.
///
/// The bundle is what the runner gets, so this is where a backend choice stops
/// mattering: a VM bundle and a hybrid bundle are both just payloads by the time
/// they reach the wire.
pub fn build_bundle(
    program: &IrProgram,
    source: &Path,
    runner: RunnerId,
    backend: LiveBackend,
) -> Result<Bundle, LiveError> {
    match backend {
        LiveBackend::Vm => {
            let module = kira_bytecode::compile(program)
                .map_err(|error| LiveError::build(backend, &error))?;
            Ok(Bundle::build(
                runner,
                BuildProfile::Debug,
                vec![NamedPayload {
                    name: "app.kbc".to_owned(),
                    kind: PayloadKind::VmBytecode,
                    bytes: module.to_bytes(),
                }],
                0,
            ))
        }
        LiveBackend::Hybrid => build_hybrid_bundle(program, source, runner),
    }
}

/// Builds a hybrid bundle: the manifest, its bytecode, and its native library.
///
/// The manifest is the entrypoint, and the other two are the halves it names.
/// A `KHM1` manifest names them as plain file names beside itself, and a
/// bundle's payloads are staged flat in one directory — so the manifest resolves
/// inside the runner's cache exactly as it did in the build directory.
fn build_hybrid_bundle(
    program: &IrProgram,
    source: &Path,
    runner: RunnerId,
) -> Result<Bundle, LiveError> {
    let bundle = hybrid::build(program, source, false)
        .map_err(|error| LiveError::build(LiveBackend::Hybrid, &error))?;
    let artifacts = Artifacts::for_source(source).map_err(|source| LiveError::Io {
        path: PathBuf::from("."),
        source,
    })?;

    let manifest_path = bundle.manifest;
    let bytecode_path = artifacts.bytecode();
    let library_path = artifacts.shared_library();

    let payloads = vec![
        named_payload(&manifest_path, PayloadKind::HybridManifest)?,
        named_payload(&bytecode_path, PayloadKind::VmBytecode)?,
        named_payload(&library_path, PayloadKind::NativeLibrary)?,
    ];
    // The manifest is payload 0, and it is the entrypoint: it is the only payload
    // that knows how the other two fit together.
    Ok(Bundle::build(runner, BuildProfile::Debug, payloads, 0))
}

/// Reads `path` into a payload named by its file name.
fn named_payload(path: &Path, kind: PayloadKind) -> Result<NamedPayload, LiveError> {
    let bytes = std::fs::read(path).map_err(|source| LiveError::Io {
        path: path.to_owned(),
        source,
    })?;
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .ok_or_else(|| LiveError::Build {
            backend: LiveBackend::Hybrid.label(),
            reason: format!("built artifact `{}` has no file name", path.display()),
        })?;
    Ok(NamedPayload { name, kind, bytes })
}

/// Runs a whole live session and returns the process exit code.
pub fn session(program: &IrProgram, source: &Path, options: &LiveOptions) -> Result<(), LiveError> {
    if options.runner != RunnerId::Desktop {
        return Err(LiveError::NoRunnerClient {
            runner: options.runner.label(),
        });
    }

    let bundle = build_bundle(program, source, options.runner, options.backend)?;
    println!(
        "{}",
        LiveEvent::BundleBuilt {
            payloads: bundle.manifest().payloads.len(),
        }
    );

    // Port 0: the OS picks. A fixed port would collide with a previous session
    // that has not finished dying.
    let address = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0));
    let server = LiveServer::bind(address, bundle)?;
    let bound = server.local_addr()?;
    println!(
        "{}",
        LiveEvent::ServerStarted {
            address: bound.to_string(),
        }
    );

    let _runner = spawn_runner(options.runner, bound)?;

    // Headless: this runner has no window to present to, so the session's bar is
    // the entrypoint. That is a real bar, not a lowered one — see the runner's
    // own docs for why a frame cannot be claimed from this repo.
    let progress = server.serve_once(true, &mut |event| println!("{event}"))?;
    debug_assert!(
        progress.ready(true).is_ok(),
        "serve_once returns only on a ready session"
    );

    println!("{}", LiveEvent::ShutdownStarted);
    println!("{}", LiveEvent::ShutdownFinished);
    Ok(())
}

/// Starts the runner client for `runner`, pointed at `server`.
fn spawn_runner(runner: RunnerId, server: SocketAddr) -> Result<RunnerProcess, LiveError> {
    let path = runner_client_path(runner)?;
    let child = Command::new(&path)
        .arg("--server")
        .arg(server.to_string())
        .spawn()
        .map_err(|source| LiveError::Spawn {
            runner: runner.label(),
            path,
            source,
        })?;
    Ok(RunnerProcess { child })
}

/// Where the runner client for `runner` lives.
///
/// Beside this executable: the runner client ships with the toolchain that built
/// it, and a session must never pick up a runner from the PATH that came from a
/// different build than the bundle it is about to serve.
fn runner_client_path(runner: RunnerId) -> Result<PathBuf, LiveError> {
    let current = std::env::current_exe().map_err(LiveError::Locate)?;
    let directory = current.parent().unwrap_or(Path::new("."));
    let name = match runner {
        RunnerId::Desktop => "kira-desktop-runner",
        // Unreachable today: `session` refuses a non-desktop runner before it
        // gets here. Named rather than asserted so adding a runner client is a
        // matter of naming it, and a missing one is still a real error.
        other => {
            return Err(LiveError::NoRunnerClient {
                runner: other.label(),
            });
        }
    };
    Ok(directory.join(executable_name(name)))
}

/// The platform's file name for an executable called `stem`.
fn executable_name(stem: &str) -> String {
    if cfg!(target_os = "windows") {
        format!("{stem}.exe")
    } else {
        stem.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn a_live_session_defaults_to_the_desktop_runner() {
        let options = LiveOptions::parse(&args(&["app.kira"])).expect("parses");
        assert_eq!(options.runner, RunnerId::Desktop);
        assert_eq!(options.path, "app.kira");
        assert_eq!(options.backend, LiveBackend::Vm);
    }

    #[test]
    fn a_named_runner_is_parsed() {
        let options = LiveOptions::parse(&args(&["ios", "app.kira"])).expect("parses");
        assert_eq!(options.runner, RunnerId::Ios);
        assert_eq!(options.path, "app.kira");
    }

    /// `kira live ios` is a runner; `kira live ./ios` is a path. The docs are
    /// explicit about this, and it is the one place a runner id and a path can
    /// be confused.
    #[test]
    fn a_path_shaped_first_positional_is_a_path_not_a_runner() {
        for path in ["./ios", "../ios", "/tmp/ios", "ios.kira"] {
            let options = LiveOptions::parse(&args(&[path])).expect("parses");
            assert_eq!(options.runner, RunnerId::Desktop, "`{path}` is a path");
            assert_eq!(options.path, path);
        }
    }

    #[test]
    fn every_runner_id_parses() {
        for runner in RunnerId::all() {
            let options =
                LiveOptions::parse(&args(&[runner.label(), "app.kira"])).expect("every id parses");
            assert_eq!(options.runner, runner);
        }
    }

    #[test]
    fn a_backend_is_parsed() {
        let options =
            LiveOptions::parse(&args(&["--backend", "hybrid", "app.kira"])).expect("parses");
        assert_eq!(options.backend, LiveBackend::Hybrid);
    }

    #[test]
    fn an_unknown_backend_is_a_usage_error() {
        assert_eq!(
            LiveOptions::parse(&args(&["--backend", "llvm", "app.kira"])),
            Err(LiveOptionsError::UnknownBackend("llvm".to_owned()))
        );
    }

    #[test]
    fn a_session_without_a_path_is_a_usage_error() {
        assert_eq!(
            LiveOptions::parse(&args(&[])),
            Err(LiveOptionsError::NoPath)
        );
        assert_eq!(
            LiveOptions::parse(&args(&["desktop"])),
            Err(LiveOptionsError::NoPath)
        );
    }

    #[test]
    fn two_paths_are_a_usage_error() {
        assert_eq!(
            LiveOptions::parse(&args(&["a.kira", "b.kira"])),
            Err(LiveOptionsError::TooManyPaths {
                first: "a.kira".to_owned(),
                second: "b.kira".to_owned(),
            })
        );
    }

    #[test]
    fn a_backend_flag_without_a_value_is_a_usage_error() {
        assert_eq!(
            LiveOptions::parse(&args(&["--backend"])),
            Err(LiveOptionsError::MissingValue("--backend".to_owned()))
        );
    }

    /// A runner with no client says so precisely. The command parsed, the runner
    /// is real, and the diagnostic names what is missing.
    #[test]
    fn a_runner_without_a_client_is_named_precisely() {
        let error = runner_client_path(RunnerId::Android)
            .expect_err("android has no runner client in this build");
        assert!(
            matches!(error, LiveError::NoRunnerClient { runner: "android" }),
            "got {error:?}"
        );
        assert!(
            error.to_string().contains("android"),
            "the diagnostic must name the runner: {error}"
        );
    }

    #[test]
    fn the_desktop_runner_client_sits_beside_this_executable() {
        let path = runner_client_path(RunnerId::Desktop).expect("desktop has a client");
        assert_eq!(
            path.file_name().and_then(|name| name.to_str()),
            Some(executable_name("kira-desktop-runner").as_str())
        );
    }

    #[test]
    fn backend_labels_round_trip() {
        for backend in [LiveBackend::Vm, LiveBackend::Hybrid] {
            assert_eq!(LiveBackend::parse(backend.label()), Some(backend));
        }
    }
}
