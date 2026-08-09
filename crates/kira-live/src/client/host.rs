//! What a bundle *means* on a platform, and how an app's run ended.
//!
//! The half of a runner this crate cannot write. [`RunnerClient`](super::RunnerClient)
//! owns the protocol and knows nothing about loading bytecode or linking a
//! signed app; a platform implements [`RunnerHost`] and the client drives it. So
//! a new runner adds a host and nothing in the protocol changes.

use std::fmt;

use crate::store::Bundle;

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
    pub(super) fn reason(self) -> Option<String> {
        match self {
            Self::Finished => None,
            Self::Failed(reason) => Some(reason),
        }
    }
}
