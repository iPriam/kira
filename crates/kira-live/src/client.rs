//! The runner's half of a live session: fetch the bundle, load it, report.
//!
//! [`RunnerClient`] owns the protocol; [`RunnerHost`] owns what a bundle *means*
//! on a given platform. That split is the point. This crate must not know how a
//! desktop runner loads bytecode or how an Apple runner links a signed app —
//! each runner implements the trait and this drives it. So a new runner adds a
//! `RunnerHost` and nothing here changes.
//!
//! The client reports each milestone only after the host actually reached it.
//! [`RunnerClient::run_session`] calls `load`, and only if `load` returns `Ok`
//! does it report `BundleLoaded`. A host that fails reports `Failed` with its
//! reason and the session ends — it never falls through to the next milestone.

use std::fmt;
use std::io::{BufReader, BufWriter};
use std::net::{SocketAddr, TcpStream};
use std::time::Duration;

use kira_manifest::RunnerId;

use crate::bundle::BundleManifest;
use crate::event::ReloadMode;
use crate::progress::SessionPhase;
use crate::protocol::{
    ClientMessage, PROTOCOL_VERSION, ProtocolError, ServerMessage, read_message, write_message,
};
use crate::store::{Bundle, BundleError};

/// How long a runner waits on a server that has gone quiet.
pub const READ_TIMEOUT: Duration = Duration::from_secs(30);

/// How often an idle runner looks at the socket and at its app.
///
/// Small enough that a save feels immediate and an app's exit is reported at
/// once, large enough that a runner left up for hours costs nothing.
const IDLE_POLL: Duration = Duration::from_millis(20);

/// How long a runner waits for the server to accept bytes.
///
/// Bounded for the same reason as the read: a server that stops reading would
/// otherwise block this runner in `write_all` forever once the send buffer
/// fills, and a runner that cannot be killed by its own timeout is a runner that
/// outlives the session that started it.
pub const WRITE_TIMEOUT: Duration = Duration::from_secs(30);

/// What a platform actually does with a bundle.
///
/// Implemented once per runner. The three steps are separate because they fail
/// for different reasons and a session needs to say which one failed: a bundle
/// that will not load is a different problem from one that loads and will not
/// link.
pub trait RunnerHost {
    /// Why this host could not do something.
    ///
    /// An associated type because this crate cannot enumerate the failures of
    /// runners it does not know about. It crosses the wire as its `Display`
    /// text, which is the only form the other end could use anyway.
    type Error: fmt::Display;

    /// Loads the bundle's payloads into the process.
    fn load(&mut self, bundle: &Bundle) -> Result<(), Self::Error>;

    /// Links what was loaded, resolving whatever the payloads need from each
    /// other and from the host.
    fn link(&mut self) -> Result<(), Self::Error>;

    /// Starts the app's entrypoint.
    ///
    /// Returns when the entrypoint is *running*, which for an app with a run
    /// loop is not when it has finished. A host whose entrypoint outlives this
    /// call keeps it running somewhere the protocol is not: the milestone this
    /// return value feeds is `entrypoint started`, and a session that could only
    /// report it after the app exited could never report it for an app.
    fn start(&mut self) -> Result<(), Self::Error>;

    /// Runs a just-swapped entrypoint to completion.
    ///
    /// The proof behind `reload.completed`: a swap that commits and then traps
    /// on its first call is not a reload that worked, and only running the code
    /// tells the two apart. Asked exclusively after a swap, which means
    /// exclusively of a host that answered [`RunnerHost::hot_patch_refusal`] with
    /// `None` — an idle one. That is what makes waiting for the run to finish
    /// safe here and not at [`RunnerHost::start`].
    ///
    /// Defaults to [`RunnerHost::start`], which is right for any host that runs
    /// its entrypoint on the calling thread: for such a host the two are the same
    /// call.
    fn run_once(&mut self) -> Result<(), Self::Error> {
        self.start()
    }

    /// Swaps `bundle` into the running process, in place.
    ///
    /// The supervisor has already established that the swap is possible — the
    /// native half is byte-identical, so the process's loaded code is still
    /// current — and this is the host committing to it. The process, its loaded
    /// libraries, and anything they hold survive; the bytecode does not.
    ///
    /// A host that cannot take a particular swap returns an error, and the
    /// session relaunches instead. That is the honest answer and it is always
    /// available: only the host knows what its own live values depend on.
    fn swap(&mut self, bundle: &Bundle) -> Result<(), Self::Error>;

    /// How the app's run ended, if it has ended since this was last asked.
    ///
    /// Taken rather than read: the fact is reported to the server once, and a
    /// host that kept answering would report the same exit on every poll.
    ///
    /// This is the second thing a runner waits on. The server arrives over the
    /// socket and the app arrives here, and a host that never answers anything
    /// but `None` — one whose entrypoint runs on the calling thread, so its
    /// return is already an event the caller saw — is the default.
    fn take_app_exit(&mut self) -> Option<AppOutcome> {
        None
    }

    /// Why this host cannot take a hot patch, or `None` if it can.
    ///
    /// Asked before a rebuilt bundle is downloaded, so a host that was never
    /// going to swap does not pay for the payloads first. The reason crosses the
    /// wire as the session's relaunch reason, so it is written for whoever is
    /// watching the terminal.
    ///
    /// Two kinds of answer live here. The kill switch is one: a host that has it
    /// set relaunches every reload, which is what makes it possible to tell
    /// whether a bug belongs to the hot-patch path or was always there. The other
    /// is a host that is simply busy — an app still inside its run loop has a
    /// call stack in the code a swap would replace, and no runner gets to pull a
    /// module out from under one.
    fn hot_patch_refusal(&self) -> Option<String> {
        None
    }
}

/// An error running a live session from the runner's side.
#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    /// The socket failed.
    #[error("live client socket failed: {0}")]
    Io(#[from] std::io::Error),
    /// The protocol failed.
    #[error("live client protocol failed: {0}")]
    Protocol(#[from] ProtocolError),
    /// The server's first message was not a `Welcome`.
    #[error("live server did not welcome this runner")]
    NoWelcome,
    /// The server speaks a protocol version this build does not.
    #[error("live server speaks protocol {theirs}, this runner speaks {ours}")]
    VersionMismatch {
        /// The version the server announced.
        theirs: u16,
        /// The version this build speaks.
        ours: u16,
    },
    /// The server sent something other than what was asked for.
    #[error("live server sent an unexpected message while {while_doing}")]
    Unexpected {
        /// What the runner was doing when the message arrived.
        while_doing: &'static str,
    },
    /// The server had no payload its own manifest names.
    #[error("live server has no payload `{name}`, which its manifest names")]
    MissingPayload {
        /// The payload's name.
        name: String,
    },
    /// The bundle did not survive verification against its manifest.
    #[error("live bundle did not verify: {0}")]
    Bundle(#[from] BundleError),
    /// The bundle's manifest did not decode.
    #[error("live bundle manifest did not decode: {0}")]
    Manifest(#[from] crate::bundle::BundleDecodeError),
    /// The server ended the session.
    #[error("live server shut the session down")]
    ShutDown,
    /// The host could not load, link, or start the bundle.
    #[error("runner could not {step} the bundle: {reason}")]
    Host {
        /// Which step failed.
        step: &'static str,
        /// What the host said about it.
        reason: String,
    },
}

/// How an app's own run ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppOutcome {
    /// The entrypoint returned.
    Finished,
    /// The entrypoint stopped because it failed.
    Failed(String),
}

impl AppOutcome {
    /// The reason to report, or `None` for an app that simply finished.
    fn reason(self) -> Option<String> {
        match self {
            Self::Finished => None,
            Self::Failed(reason) => Some(reason),
        }
    }
}

/// A runner's connection to a live server.
#[derive(Debug)]
pub struct RunnerClient {
    reader: BufReader<TcpStream>,
    writer: BufWriter<TcpStream>,
    /// The bundle the runner currently holds, kept so a reload can reuse the
    /// payloads that did not change rather than re-downloading them.
    loaded: Option<Bundle>,
}

impl RunnerClient {
    /// Connects to a live server and completes the handshake.
    ///
    /// Returns once the server has welcomed this runner, so a returned client is
    /// one the server has agreed to serve.
    pub fn connect(address: SocketAddr, runner: RunnerId) -> Result<RunnerClient, ClientError> {
        let stream = TcpStream::connect(address)?;
        stream.set_read_timeout(Some(READ_TIMEOUT))?;
        stream.set_write_timeout(Some(WRITE_TIMEOUT))?;
        stream.set_nodelay(true)?;
        let mut client = RunnerClient {
            reader: BufReader::new(stream.try_clone()?),
            writer: BufWriter::new(stream),
            loaded: None,
        };
        client.handshake(runner)?;
        Ok(client)
    }

    /// Sends `Hello` and waits for `Welcome`.
    fn handshake(&mut self, runner: RunnerId) -> Result<(), ClientError> {
        write_message(
            &mut self.writer,
            &ClientMessage::Hello {
                protocol: PROTOCOL_VERSION,
                runner,
            },
        )?;
        match read_message(&mut self.reader)? {
            ServerMessage::Welcome { protocol } if protocol == PROTOCOL_VERSION => Ok(()),
            ServerMessage::Welcome { protocol } => Err(ClientError::VersionMismatch {
                theirs: protocol,
                ours: PROTOCOL_VERSION,
            }),
            ServerMessage::Shutdown => Err(ClientError::ShutDown),
            _ => Err(ClientError::NoWelcome),
        }
    }

    /// Downloads the bundle: its manifest, then every payload the manifest names.
    ///
    /// The returned bundle has been verified against its manifest — every
    /// payload hashed and checked — so a caller holds bytes that are what the
    /// build produced or holds an error, never something in between.
    pub fn fetch_bundle(&mut self) -> Result<Bundle, ClientError> {
        write_message(&mut self.writer, &ClientMessage::RequestBundle)?;
        let manifest = match read_message(&mut self.reader)? {
            ServerMessage::Manifest { bytes } => BundleManifest::from_bytes(&bytes)?,
            ServerMessage::Shutdown => return Err(ClientError::ShutDown),
            _ => {
                return Err(ClientError::Unexpected {
                    while_doing: "asking for the bundle manifest",
                });
            }
        };
        let bundle = self.download(manifest)?;
        // Remembered so a later reload can reuse what did not change.
        self.loaded = Some(bundle.clone());
        Ok(bundle)
    }

    /// Reports that a milestone actually happened.
    pub fn report(&mut self, phase: SessionPhase) -> Result<(), ClientError> {
        write_message(&mut self.writer, &ClientMessage::Progress { phase })?;
        Ok(())
    }

    /// Reports that the app's entrypoint returned, and why if it did not finish.
    ///
    /// The runner stays connected across this: the app is over, the runner is
    /// not, and which of those ends the session is the server's to decide.
    pub fn report_app_exited(&mut self, outcome: AppOutcome) -> Result<(), ClientError> {
        write_message(
            &mut self.writer,
            &ClientMessage::AppExited {
                reason: outcome.reason(),
            },
        )?;
        Ok(())
    }

    /// Reports that this runner could not continue, and why.
    pub fn fail(&mut self, reason: &str) -> Result<(), ClientError> {
        write_message(
            &mut self.writer,
            &ClientMessage::Failed {
                reason: reason.to_owned(),
            },
        )?;
        Ok(())
    }

    /// Ends the session cleanly.
    pub fn goodbye(&mut self) -> Result<(), ClientError> {
        write_message(&mut self.writer, &ClientMessage::Goodbye)?;
        Ok(())
    }

    /// Runs a whole session: download the bundle, drive `host` through it, and
    /// report each milestone the host actually reaches.
    ///
    /// A host failure is reported to the server before it is returned, so the
    /// server's session ends with the runner's reason rather than with a bare
    /// disconnect it would have to guess about.
    pub fn run_session<H: RunnerHost>(&mut self, host: &mut H) -> Result<Bundle, ClientError> {
        let bundle = self.prepare_session(host)?;
        self.start_entrypoint(host)?;
        Ok(bundle)
    }

    /// Downloads the bundle and gets `host` as far as linked, reporting each
    /// milestone the host actually reaches.
    ///
    /// Split from the start so a runner can put the entrypoint on a different
    /// thread from the protocol. An app with a run loop owns whichever thread
    /// starts it for as long as it lives, and a runner that started one on the
    /// thread holding the socket would have no way left to hear a reload.
    pub fn prepare_session<H: RunnerHost>(&mut self, host: &mut H) -> Result<Bundle, ClientError> {
        let bundle = self.fetch_bundle()?;
        self.report(SessionPhase::BundleReceived)?;

        self.step("load", host.load(&bundle))?;
        self.report(SessionPhase::BundleLoaded)?;

        self.step("link", host.link())?;
        self.report(SessionPhase::BundleLinked)?;

        Ok(bundle)
    }

    /// Starts the linked bundle's entrypoint and reports that it is running.
    pub fn start_entrypoint<H: RunnerHost>(&mut self, host: &mut H) -> Result<(), ClientError> {
        self.step("start", host.start())?;
        self.report(SessionPhase::EntrypointStarted)?;
        Ok(())
    }

    /// Stays connected, taking reloads until the server ends the session.
    ///
    /// This is what makes a runner a runner rather than a one-shot: the app is
    /// up, and the process waits to be told that a rebuilt bundle exists.
    ///
    /// Each reload is downloaded, staged, swapped, and run — and each of those
    /// four is reported only after it actually happened. `ReloadApplied` goes out
    /// after the swap returns; `ReloadCompleted` only after the new code has run
    /// once without incident, which is why the two are separate. A swap that
    /// commits and then traps on its first call is not a reload that worked, and
    /// the session must be able to tell the difference.
    pub fn serve_reloads<H: RunnerHost>(&mut self, host: &mut H) -> Result<(), ClientError> {
        loop {
            self.wait_for_the_server_or_the_app(host)?;

            // Waiting for a save is unbounded, and must be. A read timeout here
            // would make the runner kill itself for the crime of the developer
            // thinking for half a minute — the app would be gone by the time they
            // saved, and every reload after an idle gap would silently degrade to
            // a relaunch. Nothing is lost by waiting forever: a server that dies
            // closes the socket, and a closed socket is a read of zero bytes, not
            // a hang. That is the `Disconnected` arm below.
            self.set_read_timeout(None)?;
            let message: ServerMessage = match read_message(&mut self.reader) {
                Ok(message) => message,
                // The server went away without saying goodbye. The session is
                // over either way; a runner outliving its server is an orphan.
                Err(ProtocolError::Disconnected) => return Ok(()),
                Err(error) => return Err(error.into()),
            };

            // Bounded again for everything that follows: once the server has
            // spoken, the rest of the exchange is a conversation it is driving,
            // and a peer that stops mid-download must not wedge this process.
            self.set_read_timeout(Some(READ_TIMEOUT))?;

            match message {
                ServerMessage::Shutdown => return Ok(()),
                ServerMessage::Reload { mode, manifest } => {
                    self.apply_reload(host, mode, &manifest)?;
                }
                _ => {
                    return Err(ClientError::Unexpected {
                        while_doing: "waiting for a reload",
                    });
                }
            }
        }
    }

    /// Waits until the server has said something, reporting the app's own exit
    /// if that is what happens first.
    ///
    /// An idle runner is waiting on two things at once and only one of them
    /// arrives on the socket. So the wait is a poll rather than a block: the
    /// socket is checked without consuming anything, and between checks the host
    /// is asked whether the app has ended. Both waits stay unbounded — this
    /// returns when the server speaks, and everything else it notices on the way
    /// is reported and waited past.
    fn wait_for_the_server_or_the_app<H: RunnerHost>(
        &mut self,
        host: &mut H,
    ) -> Result<(), ClientError> {
        while !self.server_has_spoken()? {
            if let Some(outcome) = host.take_app_exit() {
                self.report_app_exited(outcome)?;
            }
            std::thread::sleep(IDLE_POLL);
        }
        Ok(())
    }

    /// Whether a message is waiting, without taking any of it off the socket.
    ///
    /// Peeked rather than read with a short timeout, because a timeout that
    /// fired between a frame's length prefix and its body would leave the stream
    /// desynchronized — and a runner that guessed at the rest of a message is
    /// worse than one that waits.
    fn server_has_spoken(&mut self) -> Result<bool, ClientError> {
        // Buffered bytes never come back to the socket, so a peek that looked
        // only at the socket could sit forever on a message already in hand.
        if !self.reader.buffer().is_empty() {
            return Ok(true);
        }
        let socket = self.reader.get_ref();
        socket.set_nonblocking(true)?;
        let mut byte = [0u8; 1];
        let spoken = match socket.peek(&mut byte) {
            // Zero bytes is the peer having closed, which the read that follows
            // reports as a disconnect — this only has to say that waiting is over.
            Ok(_) => Ok(true),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => Ok(false),
            Err(error) => Err(ClientError::Io(error)),
        };
        socket.set_nonblocking(false)?;
        spoken
    }

    /// Sets how long a read waits, or `None` to wait indefinitely.
    fn set_read_timeout(&mut self, timeout: Option<Duration>) -> Result<(), ClientError> {
        self.reader.get_ref().set_read_timeout(timeout)?;
        Ok(())
    }

    /// Takes one reload, reporting each step as it actually happens.
    fn apply_reload<H: RunnerHost>(
        &mut self,
        host: &mut H,
        mode: ReloadMode,
        manifest: &[u8],
    ) -> Result<(), ClientError> {
        // A relaunch is not something a runner does to itself: the supervisor
        // replaces the process. Being asked to relaunch in place is a server that
        // has confused the two tiers, and doing nothing quietly would leave it
        // waiting forever.
        if mode == ReloadMode::Relaunch {
            self.restart_required("a runner cannot relaunch itself in place")?;
            return Ok(());
        }
        if let Some(reason) = host.hot_patch_refusal() {
            self.restart_required(&reason)?;
            return Ok(());
        }

        let manifest = BundleManifest::from_bytes(manifest)?;
        // Only what actually changed comes over the wire. A hot patch's whole
        // premise is that the native library is byte-identical, so re-sending it
        // would mean shipping megabytes to prove they are the same megabytes —
        // the payload hashes exist exactly so that nobody has to.
        let bundle = match self.download_reusing(manifest) {
            Ok(bundle) => bundle,
            // A bundle that will not download is not a bundle to swap to, and
            // the running app is still fine — so this is a restart requirement,
            // not a session failure.
            Err(error) => {
                self.restart_required(&format!("the rebuilt bundle did not arrive: {error}"))?;
                return Ok(());
            }
        };

        write_message(&mut self.writer, &ClientMessage::ReloadStaged)?;

        if let Err(error) = host.swap(&bundle) {
            let reason = error.to_string();
            write_message(&mut self.writer, &ClientMessage::ReloadRejected { reason })?;
            return Ok(());
        }
        write_message(&mut self.writer, &ClientMessage::ReloadApplied)?;

        // The swap is committed; now prove it runs. A trap here is a rejection
        // of the reload rather than a failure of the session: the supervisor
        // relaunches, and the developer sees the trap.
        if let Err(error) = host.run_once() {
            let reason = format!("the swapped-in code did not run: {error}");
            write_message(&mut self.writer, &ClientMessage::ReloadRejected { reason })?;
            return Ok(());
        }
        write_message(&mut self.writer, &ClientMessage::ReloadCompleted)?;
        Ok(())
    }

    /// Tells the server this runner must be replaced, and why.
    fn restart_required(&mut self, reason: &str) -> Result<(), ClientError> {
        write_message(
            &mut self.writer,
            &ClientMessage::RestartRequired {
                reason: reason.to_owned(),
            },
        )?;
        Ok(())
    }

    /// Downloads `manifest`'s payloads, reusing any the runner already holds.
    ///
    /// A payload is reused only when its hash matches, and the reused bytes go
    /// through the same verification as downloaded ones — so a reused payload is
    /// exactly the payload the manifest names, or the bundle is refused.
    fn download_reusing(&mut self, manifest: BundleManifest) -> Result<Bundle, ClientError> {
        let held = self.loaded.take();
        let mut payloads = Vec::with_capacity(manifest.payloads.len());
        for entry in &manifest.payloads {
            let reused = held.as_ref().and_then(|bundle| {
                let previous = bundle.manifest().payload(&entry.name)?;
                (previous.hash == entry.hash)
                    .then(|| bundle.payload_by_name(&entry.name))
                    .flatten()
                    .map(<[u8]>::to_vec)
            });
            if let Some(bytes) = reused {
                payloads.push(Some(bytes));
                continue;
            }

            write_message(
                &mut self.writer,
                &ClientMessage::RequestPayload {
                    name: entry.name.clone(),
                },
            )?;
            payloads.push(Some(self.receive_payload(&entry.name)?));
        }
        let bundle = Bundle::assemble(manifest, payloads)?;
        self.loaded = Some(bundle.clone());
        Ok(bundle)
    }

    /// Reads one payload the server was asked for by `name`.
    fn receive_payload(&mut self, name: &str) -> Result<Vec<u8>, ClientError> {
        match read_message(&mut self.reader)? {
            ServerMessage::Payload { name: got, bytes } if got == name => Ok(bytes),
            // A payload arriving under a name that was not asked for would
            // desync the manifest from the bytes, so it is refused rather than
            // stored at whatever index happens to be next.
            ServerMessage::Payload { .. } => Err(ClientError::Unexpected {
                while_doing: "downloading payloads",
            }),
            ServerMessage::NoSuchPayload { name } => Err(ClientError::MissingPayload { name }),
            ServerMessage::Shutdown => Err(ClientError::ShutDown),
            _ => Err(ClientError::Unexpected {
                while_doing: "downloading payloads",
            }),
        }
    }

    /// Downloads every payload `manifest` names and verifies them against it.
    fn download(&mut self, manifest: BundleManifest) -> Result<Bundle, ClientError> {
        let mut payloads = Vec::with_capacity(manifest.payloads.len());
        for entry in &manifest.payloads {
            write_message(
                &mut self.writer,
                &ClientMessage::RequestPayload {
                    name: entry.name.clone(),
                },
            )?;
            match read_message(&mut self.reader)? {
                ServerMessage::Payload { name, bytes } if name == entry.name => {
                    payloads.push(Some(bytes));
                }
                // A payload arriving under a name that was not asked for would
                // desync the manifest from the bytes, so it is refused rather
                // than stored at whatever index happens to be next.
                ServerMessage::Payload { .. } => {
                    return Err(ClientError::Unexpected {
                        while_doing: "downloading payloads",
                    });
                }
                ServerMessage::NoSuchPayload { name } => {
                    return Err(ClientError::MissingPayload { name });
                }
                ServerMessage::Shutdown => return Err(ClientError::ShutDown),
                _ => {
                    return Err(ClientError::Unexpected {
                        while_doing: "downloading payloads",
                    });
                }
            }
        }
        Ok(Bundle::assemble(manifest, payloads)?)
    }

    /// Maps a host step's result, telling the server about a failure first.
    fn step<E: fmt::Display>(
        &mut self,
        step: &'static str,
        result: Result<(), E>,
    ) -> Result<(), ClientError> {
        match result {
            Ok(()) => Ok(()),
            Err(error) => {
                let reason = error.to_string();
                // Best-effort: the session is already failing, and a send that
                // also fails must not replace the host's reason with a socket
                // error that says nothing about why the app did not run.
                let _ = self.fail(&format!("{step}: {reason}"));
                Err(ClientError::Host { step, reason })
            }
        }
    }
}
