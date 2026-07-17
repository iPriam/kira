//! The live server: it holds the bundle and serves it to a runner.
//!
//! The server is deliberately incurious about what the runner does with the
//! bundle. It hands over the manifest, hands over each payload the runner asks
//! for by name, and records the milestones the runner reports. It never decides
//! that the app loaded — only the runner can know that, so only the runner says
//! it. The [`SessionProgress`] the server returns is a record of what the runner
//! actually reported, which is what makes it evidence rather than optimism.
//!
//! Every read is bounded by a timeout. A runner that connects and then says
//! nothing must fail the session, not hang the build.

use std::io::{BufReader, BufWriter};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::time::{Duration, Instant};

use crate::event::{LiveEvent, ProgressError, SessionPhase, SessionProgress};
use crate::protocol::{
    ClientMessage, PROTOCOL_VERSION, ProtocolError, ServerMessage, read_message, write_message,
};
use crate::store::Bundle;

/// How long the server waits on a runner that has gone quiet.
///
/// This bounds every read, so a wedged or absent runner ends the session with a
/// timeout instead of hanging whatever is driving the build.
pub const READ_TIMEOUT: Duration = Duration::from_secs(30);

/// How long the server waits for a runner to accept bytes.
///
/// A read timeout alone does not bound a session. A runner that connects, asks
/// for a payload, and then never reads leaves the server blocked in `write_all`
/// once the socket's send buffer fills — quiet in a way no read timeout
/// notices. Bounding writes too is what makes the session's promise ("a runner
/// that says nothing fails the session, not hangs the build") actually hold.
pub const WRITE_TIMEOUT: Duration = Duration::from_secs(30);

/// How long the server waits for a runner to connect at all.
///
/// The runner is a child process the session just started. If it dies before
/// connecting — a bad binary, a missing library, a kill — nothing else will ever
/// arrive on this listener, and an unbounded `accept` would hang the build
/// forever rather than report that the runner never showed up.
pub const ACCEPT_TIMEOUT: Duration = Duration::from_secs(30);

/// An error running a live session from the server's side.
#[derive(Debug, thiserror::Error)]
pub enum ServerError {
    /// The listening socket failed.
    #[error("live server socket failed: {0}")]
    Io(#[from] std::io::Error),
    /// The protocol failed.
    #[error("live server protocol failed: {0}")]
    Protocol(#[from] ProtocolError),
    /// The runner's first message was not a `Hello`.
    #[error("live runner did not introduce itself before asking for a bundle")]
    NoHello,
    /// The runner speaks a protocol version this build does not.
    #[error("live runner speaks protocol {theirs}, this build speaks {ours}")]
    VersionMismatch {
        /// The version the runner announced.
        theirs: u16,
        /// The version this build speaks.
        ours: u16,
    },
    /// The runner asked for a bundle built for a different runner.
    ///
    /// A macOS runner loading an Android bundle would fail confusingly deep
    /// inside a load, so it is refused here where the reason is still legible.
    #[error("bundle is built for the `{expected}` runner, but a `{actual}` runner connected")]
    RunnerMismatch {
        /// The runner the bundle was built for.
        expected: &'static str,
        /// The runner that actually connected.
        actual: &'static str,
    },
    /// The runner reported a milestone out of order.
    #[error("live runner reported milestones out of order: {0}")]
    Progress(#[from] ProgressError),
    /// The runner reported a milestone that is the server's to observe.
    ///
    /// Refused rather than believed: a runner that could report `bundle sent`
    /// could drive a session to ready without a bundle ever leaving the server.
    #[error("live runner reported `{0}`, which is the server's milestone to observe")]
    NotRunnerMilestone(&'static str),
    /// The runner reported that it could not continue.
    #[error("live runner failed: {0}")]
    RunnerFailed(String),
    /// The runner disconnected before the session was ready.
    #[error("live runner disconnected before the session was ready: {0}")]
    Incomplete(#[source] ProgressError),
    /// No runner connected before the session gave up waiting.
    ///
    /// Reported rather than waited on forever: the runner is a process the
    /// session started, and one that never arrives means it died on the way.
    #[error("no live runner connected within {}s", ACCEPT_TIMEOUT.as_secs())]
    RunnerNeverConnected,
}

/// A live server bound to a port, holding the bundle it serves.
#[derive(Debug)]
pub struct LiveServer {
    listener: TcpListener,
    bundle: Bundle,
}

impl LiveServer {
    /// Binds a server that will serve `bundle`.
    ///
    /// Pass port 0 to let the OS choose; [`LiveServer::local_addr`] reports what
    /// it chose. Tests rely on that rather than on a fixed port, so a stray
    /// process cannot make them flake.
    pub fn bind(address: SocketAddr, bundle: Bundle) -> Result<LiveServer, ServerError> {
        let listener = TcpListener::bind(address)?;
        Ok(LiveServer { listener, bundle })
    }

    /// The address the server is actually listening on.
    pub fn local_addr(&self) -> Result<SocketAddr, ServerError> {
        Ok(self.listener.local_addr()?)
    }

    /// The bundle this server serves.
    pub fn bundle(&self) -> &Bundle {
        &self.bundle
    }

    /// Accepts one runner and runs its session to completion.
    ///
    /// Returns what the runner actually reported reaching. The caller decides
    /// whether that is ready — the server does not round up.
    ///
    /// `on_event` sees each milestone as it happens, so a caller can print a
    /// live session as it runs rather than after it ends.
    pub fn serve_once(
        &self,
        headless: bool,
        on_event: &mut dyn FnMut(LiveEvent),
    ) -> Result<SessionProgress, ServerError> {
        self.serve_once_within(headless, ACCEPT_TIMEOUT, on_event)
    }

    /// Accepts one runner and runs its session, waiting at most `accept_timeout`
    /// for it to connect.
    ///
    /// [`LiveServer::serve_once`] is this with [`ACCEPT_TIMEOUT`]. The bound is a
    /// parameter so a test can prove the give-up path fires without spending the
    /// production timeout doing it — a 30-second test gets deleted, and then the
    /// bound it was guarding stops being checked at all.
    pub fn serve_once_within(
        &self,
        headless: bool,
        accept_timeout: Duration,
        on_event: &mut dyn FnMut(LiveEvent),
    ) -> Result<SessionProgress, ServerError> {
        let (stream, peer) = self.accept_before(accept_timeout)?;
        stream.set_read_timeout(Some(READ_TIMEOUT))?;
        stream.set_write_timeout(Some(WRITE_TIMEOUT))?;
        // Nagle would sit on the small control messages this protocol is mostly
        // made of, waiting for more data that the peer is blocked from sending.
        stream.set_nodelay(true)?;
        self.run_session(stream, peer, headless, on_event)
    }

    /// Accepts one connection, giving up after `timeout`.
    ///
    /// `TcpListener` has no accept timeout, so this polls a non-blocking
    /// listener. The poll interval is a compromise a live session can afford:
    /// it costs a wakeup every few milliseconds while a runner starts up, and it
    /// buys a bounded failure instead of a hung build.
    fn accept_before(&self, timeout: Duration) -> Result<(TcpStream, SocketAddr), ServerError> {
        /// How often to re-check for a connection while waiting.
        const POLL: Duration = Duration::from_millis(5);

        self.listener.set_nonblocking(true)?;
        let deadline = Instant::now() + timeout;
        let outcome = loop {
            match self.listener.accept() {
                Ok(accepted) => break Ok(accepted),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    if Instant::now() >= deadline {
                        break Err(ServerError::RunnerNeverConnected);
                    }
                    std::thread::sleep(POLL);
                }
                Err(error) => break Err(ServerError::Io(error)),
            }
        };
        // Restored whatever happened: the listener outlives this call, and a
        // non-blocking listener left behind would make a later accept spin.
        self.listener.set_nonblocking(false)?;
        let (stream, peer) = outcome?;
        // An accepted socket can inherit the listener's non-blocking mode, which
        // would turn every read in the session into a WouldBlock error.
        stream.set_nonblocking(false)?;
        Ok((stream, peer))
    }

    /// Runs one connected session.
    fn run_session(
        &self,
        stream: TcpStream,
        peer: SocketAddr,
        headless: bool,
        on_event: &mut dyn FnMut(LiveEvent),
    ) -> Result<SessionProgress, ServerError> {
        let mut reader = BufReader::new(stream.try_clone()?);
        let mut writer = BufWriter::new(stream);
        let mut progress = SessionProgress::new();

        self.handshake(&mut reader, &mut writer)?;
        on_event(LiveEvent::ClientConnected {
            peer: peer.to_string(),
        });
        progress.reach(SessionPhase::Connected)?;

        loop {
            let message: ClientMessage = match read_message(&mut reader) {
                Ok(message) => message,
                // The runner went away. Whether that is fine depends entirely on
                // whether it got the session up first, which the caller decides.
                Err(ProtocolError::Disconnected) => {
                    return match progress.ready(headless) {
                        Ok(()) => Ok(progress),
                        Err(error) => Err(ServerError::Incomplete(error)),
                    };
                }
                Err(error) => return Err(error.into()),
            };

            match message {
                ClientMessage::Hello { .. } => return Err(ServerError::NoHello),
                ClientMessage::RequestBundle => {
                    on_event(LiveEvent::BundleRequested);
                    write_message(
                        &mut writer,
                        &ServerMessage::Manifest {
                            bytes: self.bundle.manifest().to_bytes(),
                        },
                    )?;
                }
                ClientMessage::RequestPayload { name } => {
                    self.send_payload(&mut writer, &name, &mut progress, on_event)?;
                }
                ClientMessage::Progress { phase } => {
                    if !phase.reported_by_runner() {
                        return Err(ServerError::NotRunnerMilestone(phase.label()));
                    }
                    progress.reach(phase)?;
                    if let Some(event) = milestone_event(phase, &self.bundle) {
                        on_event(event);
                    }
                    if progress.ready(headless).is_ok() {
                        on_event(LiveEvent::SessionReady);
                        return Ok(progress);
                    }
                }
                ClientMessage::Failed { reason } => return Err(ServerError::RunnerFailed(reason)),
                ClientMessage::Goodbye => {
                    return match progress.ready(headless) {
                        Ok(()) => Ok(progress),
                        Err(error) => Err(ServerError::Incomplete(error)),
                    };
                }
            }
        }
    }

    /// Exchanges `Hello`/`Welcome`, rejecting a peer this server cannot serve.
    fn handshake(
        &self,
        reader: &mut BufReader<TcpStream>,
        writer: &mut BufWriter<TcpStream>,
    ) -> Result<(), ServerError> {
        let hello: ClientMessage = read_message(reader)?;
        let ClientMessage::Hello { protocol, runner } = hello else {
            return Err(ServerError::NoHello);
        };
        if protocol != PROTOCOL_VERSION {
            return Err(ServerError::VersionMismatch {
                theirs: protocol,
                ours: PROTOCOL_VERSION,
            });
        }
        let expected = self.bundle.manifest().runner;
        if runner != expected {
            return Err(ServerError::RunnerMismatch {
                expected: expected.label(),
                actual: runner.label(),
            });
        }
        write_message(
            writer,
            &ServerMessage::Welcome {
                protocol: PROTOCOL_VERSION,
            },
        )?;
        Ok(())
    }

    /// Sends the payload called `name`, or says the bundle has no such payload.
    ///
    /// Sending a payload is what `bundle sent` means, and it is the server's own
    /// milestone: this is the one place that records it, right where the bytes
    /// actually go out.
    fn send_payload(
        &self,
        writer: &mut BufWriter<TcpStream>,
        name: &str,
        progress: &mut SessionProgress,
        on_event: &mut dyn FnMut(LiveEvent),
    ) -> Result<(), ServerError> {
        match self.bundle.payload_by_name(name) {
            Some(bytes) => {
                let message = ServerMessage::Payload {
                    name: name.to_owned(),
                    bytes: bytes.to_vec(),
                };
                write_message(writer, &message)?;
                progress.reach(SessionPhase::BundleSent)?;
                on_event(LiveEvent::BundleSent { bytes: bytes.len() });
                Ok(())
            }
            // Not an error for the server: a runner asking for something absent
            // is the runner's problem, and it is told so it can report precisely.
            None => {
                write_message(
                    writer,
                    &ServerMessage::NoSuchPayload {
                        name: name.to_owned(),
                    },
                )?;
                Ok(())
            }
        }
    }
}

/// The event reporting a milestone the runner told us about, or `None` for a
/// phase that is not the runner's to report.
///
/// Returning `None` rather than inventing an event for the impossible case: the
/// caller already rejects a server-owned phase, and the alternative was an arm
/// that emitted some unrelated event to satisfy the match. The event vocabulary
/// is the session's public contract — a wrong event on it is worse than the
/// panic it was avoiding, because a panic at least does not lie.
fn milestone_event(phase: SessionPhase, bundle: &Bundle) -> Option<LiveEvent> {
    match phase {
        SessionPhase::BundleReceived => Some(LiveEvent::BundleReceived {
            payloads: bundle.manifest().payloads.len(),
        }),
        SessionPhase::BundleLoaded => Some(LiveEvent::BundleLoaded),
        SessionPhase::BundleLinked => Some(LiveEvent::BundleLinked),
        SessionPhase::EntrypointStarted => Some(LiveEvent::EntrypointStarted),
        SessionPhase::FramePresented => Some(LiveEvent::FramePresented),
        SessionPhase::Connected | SessionPhase::BundleSent => None,
    }
}
