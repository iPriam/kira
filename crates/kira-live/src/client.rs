//! The runner's half of a live session: fetch the bundle, load it, report.
//!
//! [`RunnerClient`] owns the protocol; [`RunnerHost`] owns what a bundle *means*
//! on a given platform, and lives in [`host`] for that reason. That split is the
//! point: this crate must not know how a desktop runner loads bytecode or how an
//! Apple runner links a signed app — each runner implements the trait and this
//! drives it. So a new runner adds a `RunnerHost` and nothing here changes.
//!
//! The client reports each milestone only after the host actually reached it.
//! [`RunnerClient::run_session`] calls `load`, and only if `load` returns `Ok`
//! does it report `BundleLoaded`. A host that fails reports `Failed` with its
//! reason and the session ends — it never falls through to the next milestone.

pub mod host;

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
use crate::reload::{ReloadDecision, decide};
use crate::store::{Bundle, BundleError};

pub use host::{AppOutcome, RunnerHost};

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

/// A runner's connection to a live server.
#[derive(Debug)]
pub struct RunnerClient {
    reader: BufReader<TcpStream>,
    writer: BufWriter<TcpStream>,
    /// The bundle the runner currently holds, kept so a reload can reuse the
    /// payloads that did not change rather than re-downloading them.
    loaded: Option<Bundle>,
    /// Whether the server has already gone.
    ///
    /// A session ends at the server's word, and the last thing the runner has to
    /// say — its goodbye, or the news that the app exited — is only news to a
    /// server still listening for it. Once the peer is gone those sends have
    /// nobody to reach, so they stop being attempted rather than failing: a
    /// runner that reported the end of a session it completed as a protocol
    /// error would exit non-zero for having finished.
    peer_left: bool,
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
            peer_left: false,
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
    /// not, and which of those ends the session is the server's to decide. A
    /// server that has already decided — and gone — is not a failure of this
    /// report, only the other order of the same two events.
    pub fn report_app_exited(&mut self, outcome: AppOutcome) -> Result<(), ClientError> {
        self.tell_a_listening_server(&ClientMessage::AppExited {
            reason: outcome.reason(),
        })
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
    ///
    /// A goodbye to a server that has already gone is not one that failed: the
    /// session it would have ended is over, and the only thing left to do about
    /// it is nothing.
    pub fn goodbye(&mut self) -> Result<(), ClientError> {
        self.tell_a_listening_server(&ClientMessage::Goodbye)
    }

    /// Sends something that only matters to a server still on the other end.
    ///
    /// The two messages a runner sends of its own accord once the app is up —
    /// its goodbye and the app's exit — are news for a live session, and a
    /// session whose server has ended has already stopped being one. So a peer
    /// that is gone makes these no-ops rather than errors, in both the order
    /// they can happen in: the departure already seen, and the departure this
    /// write is the first to find. Nothing else in the protocol is forgiving
    /// this way — every message the session is *waiting* on stays a failure when
    /// it cannot be delivered, which is what keeps a runner that loses its
    /// server mid-startup from looking like one that finished.
    fn tell_a_listening_server(&mut self, message: &ClientMessage) -> Result<(), ClientError> {
        if self.peer_left {
            return Ok(());
        }
        match write_message(&mut self.writer, message) {
            Ok(()) => Ok(()),
            Err(ProtocolError::Disconnected) => {
                self.peer_left = true;
                Ok(())
            }
            Err(error) => Err(error.into()),
        }
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
                Err(ProtocolError::Disconnected) => {
                    self.peer_left = true;
                    return Ok(());
                }
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
            // A peer that went away *without* a graceful shutdown is the same
            // situation as the zero-byte read above, and it is what a host
            // exiting normally looks like on Windows: the clean FIN a Unix host
            // sends arrives here as `ConnectionAborted` or `ConnectionReset`.
            // Reporting it as a socket failure made the runner exit non-zero for
            // a session that ran to completion, depending on which the OS chose.
            //
            // macOS adds a third spelling: a reset that lands between this
            // peek and the kernel reaping the connection makes `peek` answer
            // `EINVAL`. Nothing else in this call can produce one — the buffer
            // is ours — so it is the same "the peer is gone" event, and the
            // read that follows reports it properly.
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::ConnectionAborted
                        | std::io::ErrorKind::ConnectionReset
                        | std::io::ErrorKind::BrokenPipe
                ) || cfg!(target_os = "macos")
                    && error.raw_os_error() == Some(22) =>
            {
                Ok(true)
            }
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
        let Some(loaded) = self.loaded.as_ref() else {
            self.restart_required("the runner has no loaded bundle to compare against")?;
            return Ok(());
        };
        // The reload mode is a request, not proof. Recheck the manifest before
        // downloading or handing anything to the host.
        match decide(loaded.manifest(), &manifest, false) {
            ReloadDecision::HotPatch => {}
            ReloadDecision::Unchanged => {
                self.restart_required("the server requested a hot patch for an unchanged bundle")?;
                return Ok(());
            }
            ReloadDecision::Relaunch { reason } => {
                self.restart_required(&reason.to_string())?;
                return Ok(());
            }
        }
        // Only payloads whose manifest identity changed come over the wire. The
        // hash and size let the client reuse verified bytes without resending
        // them.
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
    /// A payload is reused only when its complete manifest row matches, and the
    /// reused bytes go through the same verification as downloaded ones.
    fn download_reusing(&mut self, manifest: BundleManifest) -> Result<Bundle, ClientError> {
        let held = self.loaded.take();
        let mut payloads = Vec::with_capacity(manifest.payloads.len());
        for entry in &manifest.payloads {
            let reused = held.as_ref().and_then(|bundle| {
                let previous = bundle.manifest().payload(&entry.name)?;
                (previous == entry)
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, SocketAddrV4, TcpListener};

    /// A host that is never asked to do anything: these are tests about the
    /// protocol, not about what a bundle means on a platform.
    struct NoHost;

    impl RunnerHost for NoHost {
        type Error = String;

        fn load(&mut self, _bundle: &Bundle) -> Result<(), String> {
            Ok(())
        }

        fn link(&mut self) -> Result<(), String> {
            Ok(())
        }

        fn start(&mut self) -> Result<(), String> {
            Ok(())
        }

        fn swap(&mut self, _bundle: &Bundle) -> Result<(), String> {
            Ok(())
        }
    }

    /// Welcomes one runner and then goes away, which is a server ending a
    /// session the only way a socket can express it.
    fn a_server_that_welcomes_and_leaves() -> (SocketAddr, std::thread::JoinHandle<()>) {
        let listener = TcpListener::bind(SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0)))
            .expect("bind");
        let address = listener.local_addr().expect("addr");
        let served = std::thread::spawn(move || {
            let (stream, _) = listener.accept().expect("accept");
            let mut reader = BufReader::new(stream.try_clone().expect("clone"));
            let mut writer = BufWriter::new(stream);
            let hello: ClientMessage = read_message(&mut reader).expect("hello");
            assert!(matches!(hello, ClientMessage::Hello { .. }));
            write_message(
                &mut writer,
                &ServerMessage::Welcome {
                    protocol: PROTOCOL_VERSION,
                },
            )
            .expect("welcome");
        });
        (address, served)
    }

    /// The end of a session is not a failure of one. A runner whose server has
    /// gone still has a goodbye to say and an app exit to report, and neither
    /// has anywhere to go — so both succeed at doing nothing rather than failing
    /// the run that already happened.
    #[test]
    fn a_runner_whose_server_left_still_ends_cleanly() {
        let (address, served) = a_server_that_welcomes_and_leaves();
        let mut client = RunnerClient::connect(address, RunnerId::Desktop).expect("connect");
        served.join().expect("the server thread does not panic");

        client
            .serve_reloads(&mut NoHost)
            .expect("a server that leaves ends the session rather than breaking it");
        assert!(
            client.peer_left,
            "the departure is remembered, not re-tried"
        );
        client
            .goodbye()
            .expect("a goodbye to nobody is not a failure");
        client
            .report_app_exited(AppOutcome::Finished)
            .expect("an app exit reported to nobody is not a failure");
    }

    /// The other half of the same rule: a server that leaves *before* the runner
    /// has what it needs is a failure, and stays one. Tolerating the end of a
    /// session must not tolerate never having had one.
    #[test]
    fn a_server_that_leaves_before_the_bundle_fails_the_session() {
        let (address, served) = a_server_that_welcomes_and_leaves();
        let mut client = RunnerClient::connect(address, RunnerId::Desktop).expect("connect");
        served.join().expect("the server thread does not panic");

        let error = client
            .fetch_bundle()
            .expect_err("a bundle that never arrives is a failed session");
        assert!(
            matches!(error, ClientError::Protocol(ProtocolError::Disconnected)),
            "got {error:?}"
        );
    }
}
