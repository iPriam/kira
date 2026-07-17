//! The observable vocabulary of a live session.
//!
//! A [`LiveEvent`] is the session's public contract: tools, tests, and people
//! read these and nothing else to know what happened. Two rules keep them
//! trustworthy.
//!
//! **An event is emitted only where its milestone actually happened.** The
//! server emits `BundleSent` when the bytes went out; only the *runner* can emit
//! `BundleLoaded`, because only the runner knows whether it loaded. A milestone
//! is never inferred from the step before it — that inference is exactly how a
//! session comes to report success for an app that never started.
//!
//! **Reaching ready is checked, not assumed.** [`SessionProgress`] tracks the
//! milestones a session has actually observed and refuses to call a session
//! ready until each required one has arrived, in order. A session that skips a
//! milestone fails rather than rounding up.
//!
//! Be precise about what that buys, because it is easy to overclaim. The server
//! enforces *ordering and ownership*: a runner cannot report a milestone before
//! its predecessor, and cannot report one of the server's own. It does not
//! enforce *honesty* — a runner that downloads a bundle, throws it away, and
//! reports each milestone in order will be believed, because the server has no
//! way to see inside it. That is not a hole to be plugged; it is where the trust
//! boundary sits. The runner is the thing being trusted to run the app, so the
//! evidence a session is real comes from the app's own observable behavior, not
//! from the protocol. This is why the end-to-end tests assert on the app's
//! stdout rather than on the milestones.

use core::fmt;

/// A milestone in bringing a live session up, in the order it must occur.
///
/// The discriminants order the handshake: a phase may only be entered from the
/// one before it. That is what makes [`SessionProgress`] able to reject a
/// session that jumped a step.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SessionPhase {
    /// The runner connected to the server.
    Connected,
    /// The server sent the bundle.
    BundleSent,
    /// The runner received and verified the bundle.
    BundleReceived,
    /// The runner loaded the bundle's payloads.
    BundleLoaded,
    /// The runner linked the loaded payloads.
    BundleLinked,
    /// The runner started the entrypoint.
    EntrypointStarted,
    /// The runner presented a frame. Not reached by a headless session.
    FramePresented,
}

impl SessionPhase {
    /// The phase that must precede this one, or `None` for the first.
    pub fn predecessor(self) -> Option<SessionPhase> {
        match self {
            Self::Connected => None,
            Self::BundleSent => Some(Self::Connected),
            Self::BundleReceived => Some(Self::BundleSent),
            Self::BundleLoaded => Some(Self::BundleReceived),
            Self::BundleLinked => Some(Self::BundleLoaded),
            Self::EntrypointStarted => Some(Self::BundleLinked),
            Self::FramePresented => Some(Self::EntrypointStarted),
        }
    }

    /// A short label for diagnostics.
    pub fn label(self) -> &'static str {
        match self {
            Self::Connected => "connected",
            Self::BundleSent => "bundle sent",
            Self::BundleReceived => "bundle received",
            Self::BundleLoaded => "bundle loaded",
            Self::BundleLinked => "bundle linked",
            Self::EntrypointStarted => "entrypoint started",
            Self::FramePresented => "frame presented",
        }
    }

    /// This phase's wire byte.
    ///
    /// Append-only, and pinned by a test: a runner built from an older checkout
    /// reports its milestones with these bytes, and a renumber would have it
    /// silently reporting a different one.
    ///
    /// The byte lives here with the phase rather than in the protocol, for the
    /// same reason [`ReloadMode::as_byte`] does: a tag and the thing it names go
    /// together, so there is one place to look and one place to get it wrong.
    pub fn as_byte(self) -> u8 {
        match self {
            Self::Connected => 0,
            Self::BundleSent => 1,
            Self::BundleReceived => 2,
            Self::BundleLoaded => 3,
            Self::BundleLinked => 4,
            Self::EntrypointStarted => 5,
            Self::FramePresented => 6,
        }
    }

    /// The phase a wire byte names, or `None` if this build knows no such phase.
    pub fn from_byte(byte: u8) -> Option<SessionPhase> {
        match byte {
            0 => Some(Self::Connected),
            1 => Some(Self::BundleSent),
            2 => Some(Self::BundleReceived),
            3 => Some(Self::BundleLoaded),
            4 => Some(Self::BundleLinked),
            5 => Some(Self::EntrypointStarted),
            6 => Some(Self::FramePresented),
            _ => None,
        }
    }

    /// Whether this milestone is the runner's to report, rather than the
    /// server's to observe.
    ///
    /// The split is who can actually know. The server knows a runner connected
    /// and knows it put bytes on the socket; it cannot know they were loaded.
    /// The runner knows it loaded them and cannot be told otherwise. So each
    /// milestone has exactly one end entitled to claim it, and a server rejects
    /// a runner that reports one of the server's own — otherwise a runner could
    /// assert its way to a ready session without a bundle ever being served.
    pub fn reported_by_runner(self) -> bool {
        match self {
            Self::Connected | Self::BundleSent => false,
            Self::BundleReceived
            | Self::BundleLoaded
            | Self::BundleLinked
            | Self::EntrypointStarted
            | Self::FramePresented => true,
        }
    }
}

/// Why a session could not be called ready.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProgressError {
    /// A milestone arrived before the one it depends on.
    #[error("live session reported `{reached}` before `{missing}`")]
    OutOfOrder {
        /// The milestone that arrived.
        reached: &'static str,
        /// The milestone that should have come first.
        missing: &'static str,
    },
    /// The session was asked whether it is ready before it had got there.
    #[error("live session is not ready: it reached `{reached}` but never `{missing}`")]
    NotReached {
        /// The furthest milestone the session actually reached.
        reached: &'static str,
        /// The milestone it never reached.
        missing: &'static str,
    },
    /// Nothing happened at all.
    #[error("live session is not ready: the runner never connected")]
    NeverStarted,
}

/// Tracks how far a live session has actually got.
///
/// The point is [`SessionProgress::ready`]: a session is ready only once every
/// required milestone has been observed, in order. Without this the "ready"
/// signal would be whatever the last hopeful log line said.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SessionProgress {
    reached: Option<SessionPhase>,
}

impl SessionProgress {
    /// A session that has not started.
    pub fn new() -> SessionProgress {
        SessionProgress { reached: None }
    }

    /// Records that `phase` was observed, rejecting a milestone that arrived
    /// before the one it depends on.
    pub fn reach(&mut self, phase: SessionPhase) -> Result<(), ProgressError> {
        if let Some(required) = phase.predecessor()
            && self.reached < Some(required)
        {
            return Err(ProgressError::OutOfOrder {
                reached: phase.label(),
                missing: required.label(),
            });
        }
        // A phase is only ever advanced: a duplicate or stale report cannot walk
        // the session backwards.
        if self.reached < Some(phase) {
            self.reached = Some(phase);
        }
        Ok(())
    }

    /// The furthest milestone reached, if any.
    pub fn reached(self) -> Option<SessionPhase> {
        self.reached
    }

    /// Whether the session has reached `phase`.
    pub fn has_reached(self, phase: SessionPhase) -> bool {
        self.reached >= Some(phase)
    }

    /// Confirms the session is ready, or says which milestone it never reached.
    ///
    /// A windowed session must have presented a frame: an app that started and
    /// then rendered nothing is not a working app, and accepting
    /// `EntrypointStarted` as ready is precisely the "it launched, so it works"
    /// claim this refuses to make. A `headless` session has no surface to
    /// present to, so it stops at the entrypoint — which is why headless is for
    /// testing the protocol, not for standing in for a real session.
    pub fn ready(self, headless: bool) -> Result<(), ProgressError> {
        let required = if headless {
            SessionPhase::EntrypointStarted
        } else {
            SessionPhase::FramePresented
        };
        if self.has_reached(required) {
            return Ok(());
        }
        match self.reached {
            None => Err(ProgressError::NeverStarted),
            Some(reached) => Err(ProgressError::NotReached {
                reached: reached.label(),
                missing: required.label(),
            }),
        }
    }
}

/// Something that happened in a live session.
///
/// The names match the session vocabulary a live session prints (`live.*`), and
/// [`LiveEvent::name`] is that name — so a test asserts on a value here rather
/// than on a substring of a log line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LiveEvent {
    /// The server bound its port and is listening.
    ServerStarted {
        /// The address the server is listening on.
        address: String,
    },
    /// A bundle finished building.
    BundleBuilt {
        /// How many payloads it holds.
        payloads: usize,
    },
    /// A runner connected.
    ClientConnected {
        /// The address it connected from.
        peer: String,
    },
    /// The runner asked for the bundle.
    BundleRequested,
    /// The server sent the bundle.
    BundleSent {
        /// How many bytes of payload went out.
        bytes: usize,
    },
    /// The runner received the bundle and verified it against its manifest.
    BundleReceived {
        /// How many payloads arrived.
        payloads: usize,
    },
    /// The runner loaded the bundle's payloads.
    BundleLoaded,
    /// The runner linked the loaded payloads.
    BundleLinked,
    /// The runner started the app's entrypoint.
    EntrypointStarted,
    /// The runner presented a frame.
    FramePresented,
    /// Every milestone required for this session has been observed.
    SessionReady,
    /// A watched source file changed.
    SourceChanged {
        /// The path that changed.
        path: String,
    },
    /// A bundle was rebuilt after a change.
    BundleRebuilt,
    /// The supervisor told the runner a rebuilt bundle exists, and which tier it
    /// is attempting.
    ///
    /// A request, not an outcome: this is emitted before the runner has done
    /// anything, and a session that stopped here would have changed nothing.
    ReloadNotified {
        /// The tier being attempted.
        mode: ReloadMode,
        /// Why, when the tier is a fallback rather than the first attempt.
        reason: Option<String>,
    },
    /// The runner loaded the rebuilt bundle, but has not swapped to it.
    ///
    /// Loaded, not live: the new code is in the process and the old code is
    /// still what runs.
    ReloadStaged,
    /// The runner swapped to the staged bundle.
    ///
    /// Committed, not proven: the new code is what runs now, but has not run
    /// yet.
    ReloadApplied,
    /// The swapped-in code ran without incident.
    ///
    /// This is the success signal, and it is deliberately later than
    /// [`LiveEvent::ReloadApplied`]: a swap that commits and then traps on its
    /// first call is not a reload that worked.
    ReloadCompleted {
        /// How the reload was applied.
        mode: ReloadMode,
    },
    /// The reload could not be applied in place and the reason it could not.
    ReloadRejected {
        /// Why the running process could not take the new bundle.
        reason: String,
    },
    /// The runner must be relaunched to take the change.
    RestartRequired {
        /// Why a relaunch is required.
        reason: String,
    },
    /// The runner process was relaunched.
    RunnerRelaunched,
    /// A clean shutdown began.
    ShutdownStarted,
    /// A clean shutdown finished.
    ShutdownFinished,
}

/// How a rebuilt bundle reaches the running app.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReloadMode {
    /// Swapped into the running process, which keeps its state.
    HotPatch,
    /// Applied by relaunching the runner, which loses its state.
    Relaunch,
}

impl ReloadMode {
    /// The label this mode is reported under.
    pub fn label(self) -> &'static str {
        match self {
            Self::HotPatch => "hotpatch",
            Self::Relaunch => "relaunch",
        }
    }

    /// This mode's wire byte. Append-only, like every tag here.
    pub fn as_byte(self) -> u8 {
        match self {
            Self::HotPatch => 0,
            Self::Relaunch => 1,
        }
    }

    /// The mode a wire byte names, or `None` if this build knows no such mode.
    pub fn from_byte(byte: u8) -> Option<ReloadMode> {
        match byte {
            0 => Some(Self::HotPatch),
            1 => Some(Self::Relaunch),
            _ => None,
        }
    }
}

impl LiveEvent {
    /// The event's name in the `live.*` vocabulary.
    pub fn name(&self) -> &'static str {
        match self {
            Self::ServerStarted { .. } => "live.server.started",
            Self::BundleBuilt { .. } => "live.bundle.built",
            Self::ClientConnected { .. } => "live.client.connected",
            Self::BundleRequested => "live.bundle.requested",
            Self::BundleSent { .. } => "live.bundle.sent",
            Self::BundleReceived { .. } => "live.bundle.received",
            Self::BundleLoaded => "live.bundle.loaded",
            Self::BundleLinked => "live.bundle.linked",
            Self::EntrypointStarted => "live.entrypoint.started",
            Self::FramePresented => "live.frame.presented",
            Self::SessionReady => "live.session.ready",
            Self::SourceChanged { .. } => "live.source.changed",
            Self::BundleRebuilt => "live.bundle.rebuilt",
            Self::ReloadNotified { .. } => "live.reload.notified",
            Self::ReloadStaged => "live.reload.staged",
            Self::ReloadApplied => "live.reload.applied",
            Self::ReloadCompleted { .. } => "live.reload.completed",
            Self::ReloadRejected { .. } => "live.reload.rejected",
            Self::RestartRequired { .. } => "live.reload.restart_required",
            Self::RunnerRelaunched => "live.runner.relaunched",
            Self::ShutdownStarted => "live.shutdown.started",
            Self::ShutdownFinished => "live.shutdown.finished",
        }
    }

    /// The milestone this event marks, for the events that mark one.
    ///
    /// This is how an event stream feeds [`SessionProgress`]: the events that
    /// are milestones map to their phase, and the rest map to `None`.
    pub fn phase(&self) -> Option<SessionPhase> {
        match self {
            Self::ClientConnected { .. } => Some(SessionPhase::Connected),
            Self::BundleSent { .. } => Some(SessionPhase::BundleSent),
            Self::BundleReceived { .. } => Some(SessionPhase::BundleReceived),
            Self::BundleLoaded => Some(SessionPhase::BundleLoaded),
            Self::BundleLinked => Some(SessionPhase::BundleLinked),
            Self::EntrypointStarted => Some(SessionPhase::EntrypointStarted),
            Self::FramePresented => Some(SessionPhase::FramePresented),
            _ => None,
        }
    }
}

/// Renders the event as its name plus its detail, which is the session's
/// on-screen form.
impl fmt::Display for LiveEvent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.name())?;
        match self {
            Self::ServerStarted { address } => write!(f, " address={address}"),
            Self::BundleBuilt { payloads } => write!(f, " payloads={payloads}"),
            Self::ClientConnected { peer } => write!(f, " peer={peer}"),
            Self::BundleSent { bytes } => write!(f, " bytes={bytes}"),
            Self::BundleReceived { payloads } => write!(f, " payloads={payloads}"),
            Self::SourceChanged { path } => write!(f, " path={path}"),
            Self::ReloadNotified { mode, reason } => {
                write!(f, " mode={}", mode.label())?;
                match reason {
                    Some(reason) => write!(f, " reason={reason}"),
                    None => Ok(()),
                }
            }
            Self::ReloadCompleted { mode } => write!(f, " mode={}", mode.label()),
            Self::ReloadRejected { reason } | Self::RestartRequired { reason } => {
                write!(f, " reason={reason}")
            }
            _ => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The full handshake, in order, reaches ready.
    fn drive(progress: &mut SessionProgress, through: SessionPhase) {
        for phase in [
            SessionPhase::Connected,
            SessionPhase::BundleSent,
            SessionPhase::BundleReceived,
            SessionPhase::BundleLoaded,
            SessionPhase::BundleLinked,
            SessionPhase::EntrypointStarted,
            SessionPhase::FramePresented,
        ] {
            if phase > through {
                break;
            }
            progress.reach(phase).expect("in-order phase");
        }
    }

    #[test]
    fn a_full_handshake_is_ready() {
        let mut progress = SessionProgress::new();
        drive(&mut progress, SessionPhase::FramePresented);
        assert_eq!(progress.ready(false), Ok(()));
    }

    /// The core anti-fake-success check: an app that started but never rendered
    /// is not a ready windowed session.
    #[test]
    fn a_started_app_that_never_rendered_is_not_ready() {
        let mut progress = SessionProgress::new();
        drive(&mut progress, SessionPhase::EntrypointStarted);
        assert_eq!(
            progress.ready(false),
            Err(ProgressError::NotReached {
                reached: "entrypoint started",
                missing: "frame presented",
            })
        );
    }

    #[test]
    fn a_headless_session_is_ready_at_the_entrypoint() {
        let mut progress = SessionProgress::new();
        drive(&mut progress, SessionPhase::EntrypointStarted);
        assert_eq!(progress.ready(true), Ok(()));
    }

    /// Headless does not lower the bar for the milestones before it.
    #[test]
    fn a_headless_session_still_needs_the_entrypoint() {
        let mut progress = SessionProgress::new();
        drive(&mut progress, SessionPhase::BundleLinked);
        assert_eq!(
            progress.ready(true),
            Err(ProgressError::NotReached {
                reached: "bundle linked",
                missing: "entrypoint started",
            })
        );
    }

    #[test]
    fn a_session_that_never_started_is_not_ready() {
        assert_eq!(
            SessionProgress::new().ready(true),
            Err(ProgressError::NeverStarted)
        );
    }

    /// A runner cannot claim to have started an app it never loaded.
    #[test]
    fn a_skipped_milestone_is_rejected() {
        let mut progress = SessionProgress::new();
        progress.reach(SessionPhase::Connected).expect("connect");
        assert_eq!(
            progress.reach(SessionPhase::EntrypointStarted),
            Err(ProgressError::OutOfOrder {
                reached: "entrypoint started",
                missing: "bundle linked",
            })
        );
    }

    #[test]
    fn the_first_milestone_needs_no_predecessor() {
        let mut progress = SessionProgress::new();
        assert_eq!(progress.reach(SessionPhase::Connected), Ok(()));
        assert_eq!(progress.reached(), Some(SessionPhase::Connected));
    }

    /// A duplicate report must not walk the session backwards and un-ready it.
    #[test]
    fn a_repeated_milestone_does_not_regress() {
        let mut progress = SessionProgress::new();
        drive(&mut progress, SessionPhase::FramePresented);
        progress
            .reach(SessionPhase::BundleLoaded)
            .expect("a stale report is accepted");
        assert_eq!(progress.reached(), Some(SessionPhase::FramePresented));
        assert_eq!(progress.ready(false), Ok(()));
    }

    /// The phase bytes are the wire contract with runners built from other
    /// checkouts, so they are pinned literally rather than left to the match arm.
    #[test]
    fn phase_wire_bytes_are_pinned() {
        let expected = [
            (SessionPhase::Connected, 0u8),
            (SessionPhase::BundleSent, 1),
            (SessionPhase::BundleReceived, 2),
            (SessionPhase::BundleLoaded, 3),
            (SessionPhase::BundleLinked, 4),
            (SessionPhase::EntrypointStarted, 5),
            (SessionPhase::FramePresented, 6),
        ];
        for (phase, byte) in expected {
            assert_eq!(phase.as_byte(), byte, "wire byte for {phase:?}");
            assert_eq!(SessionPhase::from_byte(byte), Some(phase));
        }
        assert_eq!(SessionPhase::from_byte(7), None);
    }

    /// The server owns the milestones it can actually observe; the runner owns
    /// the rest. A runner that could report the server's would be able to claim
    /// a bundle was served that never was.
    #[test]
    fn milestone_ownership_is_pinned() {
        assert!(!SessionPhase::Connected.reported_by_runner());
        assert!(!SessionPhase::BundleSent.reported_by_runner());
        assert!(SessionPhase::BundleReceived.reported_by_runner());
        assert!(SessionPhase::BundleLoaded.reported_by_runner());
        assert!(SessionPhase::BundleLinked.reported_by_runner());
        assert!(SessionPhase::EntrypointStarted.reported_by_runner());
        assert!(SessionPhase::FramePresented.reported_by_runner());
    }

    /// Every phase maps to the event that reports it, and back.
    #[test]
    fn milestone_events_map_to_their_phase() {
        let pairs = [
            (
                LiveEvent::ClientConnected {
                    peer: "127.0.0.1:1".to_owned(),
                },
                SessionPhase::Connected,
            ),
            (LiveEvent::BundleSent { bytes: 1 }, SessionPhase::BundleSent),
            (
                LiveEvent::BundleReceived { payloads: 1 },
                SessionPhase::BundleReceived,
            ),
            (LiveEvent::BundleLoaded, SessionPhase::BundleLoaded),
            (LiveEvent::BundleLinked, SessionPhase::BundleLinked),
            (
                LiveEvent::EntrypointStarted,
                SessionPhase::EntrypointStarted,
            ),
            (LiveEvent::FramePresented, SessionPhase::FramePresented),
        ];
        for (event, phase) in pairs {
            assert_eq!(event.phase(), Some(phase), "phase of {event:?}");
        }
        assert_eq!(LiveEvent::SessionReady.phase(), None);
    }

    /// The names are the documented `live.*` vocabulary, pinned so a rename is a
    /// deliberate contract change rather than a silent one.
    #[test]
    fn event_names_are_pinned() {
        assert_eq!(
            LiveEvent::ServerStarted {
                address: "127.0.0.1:0".to_owned()
            }
            .name(),
            "live.server.started"
        );
        assert_eq!(LiveEvent::SessionReady.name(), "live.session.ready");
        assert_eq!(
            LiveEvent::EntrypointStarted.name(),
            "live.entrypoint.started"
        );
        assert_eq!(LiveEvent::FramePresented.name(), "live.frame.presented");
        assert_eq!(
            LiveEvent::RestartRequired {
                reason: "native".to_owned()
            }
            .name(),
            "live.reload.restart_required"
        );
    }

    #[test]
    fn events_render_with_their_detail() {
        assert_eq!(
            LiveEvent::ServerStarted {
                address: "127.0.0.1:4000".to_owned()
            }
            .to_string(),
            "live.server.started address=127.0.0.1:4000"
        );
        assert_eq!(
            LiveEvent::ReloadCompleted {
                mode: ReloadMode::HotPatch
            }
            .to_string(),
            "live.reload.completed mode=hotpatch"
        );
        assert_eq!(LiveEvent::BundleLoaded.to_string(), "live.bundle.loaded");
        // `applied` carries no mode: it is the runner's, and a runner only ever
        // applies a hot patch — a relaunch is done to it, not by it.
        assert_eq!(LiveEvent::ReloadApplied.to_string(), "live.reload.applied");
    }

    /// The first `notified` announces the tier being attempted; the second, on a
    /// fallback, must say why. A relaunch with no reason is how someone loses
    /// their state and never learns what did it.
    #[test]
    fn a_fallback_notification_carries_its_reason() {
        assert_eq!(
            LiveEvent::ReloadNotified {
                mode: ReloadMode::HotPatch,
                reason: None,
            }
            .to_string(),
            "live.reload.notified mode=hotpatch"
        );
        assert_eq!(
            LiveEvent::ReloadNotified {
                mode: ReloadMode::Relaunch,
                reason: Some("the native library changed".to_owned()),
            }
            .to_string(),
            "live.reload.notified mode=relaunch reason=the native library changed"
        );
    }
}
