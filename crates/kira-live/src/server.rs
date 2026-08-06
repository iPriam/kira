//! The live server: it binds the port and accepts runners.
//!
//! Accepting is all it does. What happens next belongs to
//! [`LiveSession`](crate::LiveSession), because a session outlives the accept
//! that made it: a runner connects once and then takes reload after reload, and
//! a server that ended at ready would make every source change a fresh process.
//!
//! Every wait here is bounded. A runner that connects and says nothing, one that
//! stops reading, and one that never arrives at all are three different ways for
//! a session to stop making progress, and each has its own timeout — because
//! none of them is allowed to hang the build.

use std::net::{SocketAddr, TcpListener, TcpStream};
use std::time::{Duration, Instant};

use crate::event::LiveEvent;
use crate::progress::{ProgressError, SessionProgress};
use crate::protocol::ProtocolError;
use crate::session::LiveSession;
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
    /// The runner reported reload work that nobody asked it to do.
    ///
    /// Refused rather than ignored: a runner whose reports can arrive unprompted
    /// is a runner whose reports mean nothing.
    #[error("live runner reported `{0}` with no reload in flight")]
    UnexpectedReloadReport(&'static str),
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

    /// The bundle this server was bound with.
    pub fn bundle(&self) -> &Bundle {
        &self.bundle
    }

    /// Accepts one runner and runs its session to completion.
    ///
    /// Returns what the runner actually reported reaching, then ends the
    /// session. For a session that stays open — which is what reload needs — use
    /// [`LiveServer::accept_session`].
    pub fn serve_once(
        &self,
        headless: bool,
        on_event: &mut dyn FnMut(LiveEvent),
    ) -> Result<SessionProgress, ServerError> {
        self.serve_once_within(headless, ACCEPT_TIMEOUT, on_event)
    }

    /// [`LiveServer::serve_once`], waiting at most `accept_timeout` to be
    /// connected to.
    ///
    /// The bound is a parameter so a test can prove the give-up path fires
    /// without spending the production timeout doing it — a 30-second test gets
    /// deleted, and then the bound it was guarding stops being checked.
    pub fn serve_once_within(
        &self,
        headless: bool,
        accept_timeout: Duration,
        on_event: &mut dyn FnMut(LiveEvent),
    ) -> Result<SessionProgress, ServerError> {
        let session =
            self.accept_session_within(self.bundle.clone(), headless, accept_timeout, on_event)?;
        Ok(session.progress())
    }

    /// Accepts one runner, serves it `bundle`, and hands back the open session.
    ///
    /// The session is still connected when this returns, which is what makes
    /// reload possible: the runner is up, running, and listening.
    pub fn accept_session(
        &self,
        bundle: Bundle,
        headless: bool,
        on_event: &mut dyn FnMut(LiveEvent),
    ) -> Result<LiveSession, ServerError> {
        self.accept_session_within(bundle, headless, ACCEPT_TIMEOUT, on_event)
    }

    /// [`LiveServer::accept_session`], waiting at most `accept_timeout`.
    pub fn accept_session_within(
        &self,
        bundle: Bundle,
        headless: bool,
        accept_timeout: Duration,
        on_event: &mut dyn FnMut(LiveEvent),
    ) -> Result<LiveSession, ServerError> {
        let (stream, peer) = self.accept_before(accept_timeout)?;
        stream.set_read_timeout(Some(READ_TIMEOUT))?;
        stream.set_write_timeout(Some(WRITE_TIMEOUT))?;
        // Nagle would sit on the small control messages this protocol is mostly
        // made of, waiting for more data that the peer is blocked from sending.
        stream.set_nodelay(true)?;
        LiveSession::start(stream, peer, bundle, headless, on_event)
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
}
