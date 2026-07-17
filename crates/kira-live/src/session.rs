//! A connected live session: the conversation with one runner, over its life.
//!
//! [`LiveServer`](crate::LiveServer) accepts; this is what it accepts *into*. A
//! session brings a runner up to ready, then stays open — which is the whole
//! point of reload. A session that ended at ready would make every source change
//! a fresh process.
//!
//! The session is the server's half of the reload conversation, and it is
//! deliberately not the decider. [`reload`](crate::reload::decide) works out
//! which tier a rebuilt bundle deserves from the two manifests; the session
//! carries that out and reports what actually happened. When the runner refuses
//! a swap the session says so and hands the decision back — it never quietly
//! relaunches, because a relaunch that looks like a hot patch is how someone
//! loses their state and never learns why.

use std::io::{BufReader, BufWriter};
use std::net::{SocketAddr, TcpStream};

use crate::event::{LiveEvent, ReloadMode, SessionPhase, SessionProgress};
use crate::protocol::{
    ClientMessage, PROTOCOL_VERSION, ProtocolError, ServerMessage, read_message, write_message,
};
use crate::reload::{RelaunchReason, ReloadDecision, decide};
use crate::server::ServerError;
use crate::store::Bundle;

/// What happened to a rebuilt bundle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReloadOutcome {
    /// The rebuild produced the same bundle; the running app was not disturbed.
    Unchanged,
    /// The runner took the new code in place and ran it.
    HotPatched,
    /// The running process cannot take this bundle; it must be replaced.
    ///
    /// The session does not do this itself: whoever started the runner is the
    /// only one who can start another, so the decision goes back to them with
    /// the reason attached.
    NeedsRelaunch {
        /// Why a swap was not possible.
        reason: RelaunchReason,
    },
}

/// A live session with one connected runner.
#[derive(Debug)]
pub struct LiveSession {
    reader: BufReader<TcpStream>,
    writer: BufWriter<TcpStream>,
    bundle: Bundle,
    progress: SessionProgress,
    headless: bool,
    peer: SocketAddr,
}

impl LiveSession {
    /// Brings a connected runner up to a ready session.
    ///
    /// Returns once every milestone the session requires has been reported, in
    /// order, by the end entitled to report it — or an error saying which one
    /// never arrived.
    pub(crate) fn start(
        stream: TcpStream,
        peer: SocketAddr,
        bundle: Bundle,
        headless: bool,
        on_event: &mut dyn FnMut(LiveEvent),
    ) -> Result<LiveSession, ServerError> {
        let mut session = LiveSession {
            reader: BufReader::new(stream.try_clone()?),
            writer: BufWriter::new(stream),
            bundle,
            progress: SessionProgress::new(),
            headless,
            peer,
        };
        session.handshake()?;
        on_event(LiveEvent::ClientConnected {
            peer: session.peer.to_string(),
        });
        session.progress.reach(SessionPhase::Connected)?;
        session.run_to_ready(on_event)?;
        Ok(session)
    }

    /// How far the session got.
    pub fn progress(&self) -> SessionProgress {
        self.progress
    }

    /// The bundle the runner is currently running.
    pub fn bundle(&self) -> &Bundle {
        &self.bundle
    }

    /// The runner's address.
    pub fn peer(&self) -> SocketAddr {
        self.peer
    }

    /// Serves the bundle and collects milestones until the session is ready.
    fn run_to_ready(&mut self, on_event: &mut dyn FnMut(LiveEvent)) -> Result<(), ServerError> {
        loop {
            let message: ClientMessage = match read_message(&mut self.reader) {
                Ok(message) => message,
                // The runner went away before it got there. That is never a ready
                // session, whatever it managed first.
                Err(ProtocolError::Disconnected) => {
                    return Err(match self.progress.ready(self.headless) {
                        Ok(()) => {
                            ServerError::Incomplete(crate::event::ProgressError::NeverStarted)
                        }
                        Err(error) => ServerError::Incomplete(error),
                    });
                }
                Err(error) => return Err(error.into()),
            };

            match message {
                ClientMessage::Hello { .. } => return Err(ServerError::NoHello),
                ClientMessage::RequestBundle => {
                    on_event(LiveEvent::BundleRequested);
                    self.send_manifest()?;
                }
                ClientMessage::RequestPayload { name } => {
                    self.send_payload(&name, on_event)?;
                }
                ClientMessage::Progress { phase } => {
                    self.record(phase, on_event)?;
                    if self.progress.ready(self.headless).is_ok() {
                        on_event(LiveEvent::SessionReady);
                        return Ok(());
                    }
                }
                ClientMessage::Failed { reason } => return Err(ServerError::RunnerFailed(reason)),
                ClientMessage::Goodbye => {
                    return Err(match self.progress.ready(self.headless) {
                        // Goodbye before ready is a runner that gave up quietly.
                        Ok(()) => {
                            ServerError::Incomplete(crate::event::ProgressError::NeverStarted)
                        }
                        Err(error) => ServerError::Incomplete(error),
                    });
                }
                // A reload report with no reload in flight. Refused rather than
                // ignored: a runner that reports work nobody asked for is a
                // runner whose reports mean nothing.
                other => return Err(ServerError::UnexpectedReloadReport(report_name(&other))),
            }
        }
    }

    /// Records a milestone the runner reported.
    fn record(
        &mut self,
        phase: SessionPhase,
        on_event: &mut dyn FnMut(LiveEvent),
    ) -> Result<(), ServerError> {
        if !phase.reported_by_runner() {
            return Err(ServerError::NotRunnerMilestone(phase.label()));
        }
        self.progress.reach(phase)?;
        if let Some(event) = milestone_event(phase, &self.bundle) {
            on_event(event);
        }
        Ok(())
    }

    /// Offers `rebuilt` to the running runner and reports what happened.
    ///
    /// The tier is decided from the manifests, attempted, and reported. A hot
    /// patch that the runner refuses becomes a [`ReloadOutcome::NeedsRelaunch`]
    /// carrying the runner's own reason — the fallback is never silent, and the
    /// reason is never invented here.
    pub fn reload(
        &mut self,
        rebuilt: Bundle,
        hotpatch_disabled: bool,
        on_event: &mut dyn FnMut(LiveEvent),
    ) -> Result<ReloadOutcome, ServerError> {
        let decision = decide(
            self.bundle.manifest(),
            rebuilt.manifest(),
            hotpatch_disabled,
        );
        match decision {
            ReloadDecision::Unchanged => Ok(ReloadOutcome::Unchanged),
            ReloadDecision::Relaunch { reason } => {
                on_event(LiveEvent::ReloadNotified {
                    mode: ReloadMode::Relaunch,
                    reason: Some(reason.to_string()),
                });
                // The rebuilt bundle becomes this session's bundle even though
                // this session will not run it: whoever relaunches serves it, and
                // serving the old one would relaunch into the code that is
                // already stale.
                self.bundle = rebuilt;
                Ok(ReloadOutcome::NeedsRelaunch { reason })
            }
            ReloadDecision::HotPatch => {
                on_event(LiveEvent::ReloadNotified {
                    mode: ReloadMode::HotPatch,
                    reason: None,
                });
                self.bundle = rebuilt;
                self.attempt_hot_patch(on_event)
            }
        }
    }

    /// Sends the hot patch and follows the runner through it.
    fn attempt_hot_patch(
        &mut self,
        on_event: &mut dyn FnMut(LiveEvent),
    ) -> Result<ReloadOutcome, ServerError> {
        write_message(
            &mut self.writer,
            &ServerMessage::Reload {
                mode: ReloadMode::HotPatch,
                manifest: self.bundle.manifest().to_bytes(),
            },
        )?;

        loop {
            let message: ClientMessage = match read_message(&mut self.reader) {
                Ok(message) => message,
                // A runner that dies mid-swap has to be replaced; it is not a
                // runner that hot-patched.
                Err(ProtocolError::Disconnected) => {
                    return Ok(ReloadOutcome::NeedsRelaunch {
                        reason: RelaunchReason::RunnerRefused {
                            reason: "the runner disconnected while applying the reload".to_owned(),
                        },
                    });
                }
                Err(error) => return Err(error.into()),
            };

            match message {
                ClientMessage::RequestPayload { name } => self.send_payload(&name, on_event)?,
                ClientMessage::RequestBundle => self.send_manifest()?,
                ClientMessage::ReloadStaged => on_event(LiveEvent::ReloadStaged),
                ClientMessage::ReloadApplied => on_event(LiveEvent::ReloadApplied),
                ClientMessage::ReloadCompleted => {
                    on_event(LiveEvent::ReloadCompleted {
                        mode: ReloadMode::HotPatch,
                    });
                    return Ok(ReloadOutcome::HotPatched);
                }
                ClientMessage::ReloadRejected { reason } => {
                    on_event(LiveEvent::ReloadRejected {
                        reason: reason.clone(),
                    });
                    return Ok(self.fall_back(RelaunchReason::RunnerRefused { reason }, on_event));
                }
                ClientMessage::RestartRequired { reason } => {
                    on_event(LiveEvent::RestartRequired {
                        reason: reason.clone(),
                    });
                    return Ok(self.fall_back(RelaunchReason::RunnerRefused { reason }, on_event));
                }
                ClientMessage::Failed { reason } => return Err(ServerError::RunnerFailed(reason)),
                ClientMessage::Hello { .. } => return Err(ServerError::NoHello),
                // A milestone report mid-reload: the runner is re-reporting the
                // session's startup, which it has no business doing.
                ClientMessage::Progress { phase } => {
                    return Err(ServerError::UnexpectedReloadReport(phase.label()));
                }
                ClientMessage::Goodbye => {
                    return Ok(ReloadOutcome::NeedsRelaunch {
                        reason: RelaunchReason::RunnerRefused {
                            reason: "the runner shut down while applying the reload".to_owned(),
                        },
                    });
                }
            }
        }
    }

    /// Announces the fallback to relaunch and returns the outcome.
    ///
    /// The second `notified` is not noise: the first said hot patch was being
    /// attempted, and without this the session would appear to have hot-patched
    /// and then mysteriously restarted.
    fn fall_back(
        &self,
        reason: RelaunchReason,
        on_event: &mut dyn FnMut(LiveEvent),
    ) -> ReloadOutcome {
        on_event(LiveEvent::ReloadNotified {
            mode: ReloadMode::Relaunch,
            reason: Some(reason.to_string()),
        });
        ReloadOutcome::NeedsRelaunch { reason }
    }

    /// Asks the runner to shut down, and stops talking to it.
    pub fn shutdown(&mut self) -> Result<(), ServerError> {
        write_message(&mut self.writer, &ServerMessage::Shutdown)?;
        Ok(())
    }

    /// Sends the current bundle's manifest.
    fn send_manifest(&mut self) -> Result<(), ServerError> {
        write_message(
            &mut self.writer,
            &ServerMessage::Manifest {
                bytes: self.bundle.manifest().to_bytes(),
            },
        )?;
        Ok(())
    }

    /// Sends the payload called `name`, or says the bundle has no such payload.
    fn send_payload(
        &mut self,
        name: &str,
        on_event: &mut dyn FnMut(LiveEvent),
    ) -> Result<(), ServerError> {
        match self.bundle.payload_by_name(name) {
            Some(bytes) => {
                let message = ServerMessage::Payload {
                    name: name.to_owned(),
                    bytes: bytes.to_vec(),
                };
                let sent = bytes.len();
                write_message(&mut self.writer, &message)?;
                self.progress.reach(SessionPhase::BundleSent)?;
                on_event(LiveEvent::BundleSent { bytes: sent });
                Ok(())
            }
            None => {
                write_message(
                    &mut self.writer,
                    &ServerMessage::NoSuchPayload {
                        name: name.to_owned(),
                    },
                )?;
                Ok(())
            }
        }
    }

    /// Exchanges `Hello`/`Welcome`, rejecting a peer this session cannot serve.
    fn handshake(&mut self) -> Result<(), ServerError> {
        let hello: ClientMessage = read_message(&mut self.reader)?;
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
            &mut self.writer,
            &ServerMessage::Welcome {
                protocol: PROTOCOL_VERSION,
            },
        )?;
        Ok(())
    }
}

/// A name for a client message that arrived when it should not have.
fn report_name(message: &ClientMessage) -> &'static str {
    match message {
        ClientMessage::ReloadStaged => "reload staged",
        ClientMessage::ReloadApplied => "reload applied",
        ClientMessage::ReloadCompleted => "reload completed",
        ClientMessage::ReloadRejected { .. } => "reload rejected",
        ClientMessage::RestartRequired { .. } => "restart required",
        ClientMessage::Hello { .. } => "hello",
        ClientMessage::RequestBundle => "request bundle",
        ClientMessage::RequestPayload { .. } => "request payload",
        ClientMessage::Progress { .. } => "progress",
        ClientMessage::Failed { .. } => "failed",
        ClientMessage::Goodbye => "goodbye",
    }
}

/// The event reporting a milestone the runner told us about, or `None` for a
/// phase that is not the runner's to report.
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
