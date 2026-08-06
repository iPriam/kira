//! How far a live session has actually got.
//!
//! **Reaching ready is checked, not assumed.** [`SessionProgress`] tracks the
//! milestones a session has observed and refuses to call it ready until each
//! required one has arrived, in order. A session that skips a milestone fails
//! rather than rounding up.
//!
//! The ladder is here and the vocabulary that reports it is in
//! [`event`](crate::event), because the two are different contracts: a phase is
//! a wire byte a runner sends, and an event is a line a person reads.

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
}
