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
use std::time::Duration;

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
        let (stream, peer) = self.listener.accept()?;
        stream.set_read_timeout(Some(READ_TIMEOUT))?;
        // Nagle would sit on the small control messages this protocol is mostly
        // made of, waiting for more data that the peer is blocked from sending.
        stream.set_nodelay(true)?;
        self.run_session(stream, peer, headless, on_event)
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
                    on_event(milestone_event(phase, &self.bundle));
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

/// The event reporting a milestone the runner told us about.
///
/// Only the runner-reported phases appear: the server's own milestones are
/// emitted where they happen, not routed through here, and a runner cannot
/// report them at all.
fn milestone_event(phase: SessionPhase, bundle: &Bundle) -> LiveEvent {
    match phase {
        SessionPhase::BundleReceived => LiveEvent::BundleReceived {
            payloads: bundle.manifest().payloads.len(),
        },
        SessionPhase::BundleLoaded => LiveEvent::BundleLoaded,
        SessionPhase::BundleLinked => LiveEvent::BundleLinked,
        SessionPhase::EntrypointStarted => LiveEvent::EntrypointStarted,
        SessionPhase::FramePresented => LiveEvent::FramePresented,
        // Unreachable: the caller rejects a server-owned phase before getting
        // here. Reported rather than asserted away — a library never panics.
        SessionPhase::Connected | SessionPhase::BundleSent => LiveEvent::ReloadRejected {
            reason: format!("runner reported the server's `{}` milestone", phase.label()),
        },
    }
}
