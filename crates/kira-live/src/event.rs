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
//! The milestones themselves — the ladder a session climbs and the rule that it
//! may not skip a rung — are [`progress`](crate::progress).
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

use crate::progress::SessionPhase;

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
    /// The app's entrypoint returned; its runner is still up.
    ///
    /// Not the end of a session by itself. An unwatched session ends here
    /// because there is nothing left for it to do, and a watched one stays for
    /// the next save — the runner is the same either way.
    AppExited {
        /// Why it stopped, or `None` if it simply finished.
        reason: Option<String>,
    },
    /// The session captured its source baseline and is now watching for edits.
    ///
    /// Emitted once, after the watcher's baseline snapshot is taken and before
    /// the first poll. It is the moment an edit is guaranteed to be seen: a save
    /// that lands before it is folded into the baseline and never reported as a
    /// change. A tool that drives a session and then edits waits for this rather
    /// than guessing how long the initial build took.
    Watching,
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
            Self::AppExited { .. } => "live.app.exited",
            Self::Watching => "live.watch.started",
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
            Self::AppExited {
                reason: Some(reason),
            } => write!(f, " reason={reason}"),
            _ => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(LiveEvent::Watching.name(), "live.watch.started");
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
