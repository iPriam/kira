//! The live supervisor: watch, rebuild, and get the change into the running app.
//!
//! The supervisor owns the three things a session needs and a session cannot own
//! itself: the runner process, the watcher, and the ability to start a *new*
//! runner. That last one is why relaunch lives here — the session talks to a
//! runner, but only whoever started one can start another.
//!
//! The loop is small on purpose:
//!
//! ```text
//! wait for a filesystem event -> nothing changed, almost always
//! rebuild                     -> the compiler decides if the edit is even valid
//! offer it to the session     -> which decides the tier and carries it out
//! relaunch if it must         -> and say so, loudly
//! ```
//!
//! A compile error does not end the session. An editor saves a file mid-thought
//! and the program does not parse; killing a running app over that would make
//! watching worse than not watching. The diagnostics print and the old app keeps
//! running, because the last bundle that built is still the best one there is.

use std::process::{Child, Command};
use std::time::{Duration, Instant};

use kira_live::{
    Bundle, LiveEvent, LiveServer, LiveSession, ReloadOutcome, SourceWatcher, WatchSet,
};
use kira_manifest::RunnerId;

use crate::live::{LiveError, LiveOptions};

/// How often the supervisor services the runner while waiting for filesystem
/// events and checks a session deadline.
///
/// Filesystem changes wake the watcher directly; this interval is only the
/// bounded handoff between the runner socket and the filesystem channel.
const RUNNER_POLL_INTERVAL: Duration = Duration::from_millis(250);

/// How long a runner gets to exit on its own before the session kills it.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(5);

/// Builds the current source into a bundle.
///
/// `Ok(None)` means the program did not compile and its diagnostics have already
/// been reported — a session keeps running on the last bundle that did.
pub type Rebuild<'a> = &'a mut dyn FnMut() -> Result<Option<LiveBuild>, LiveError>;

/// A successful build and the source roots that produced it.
pub struct LiveBuild {
    /// The bundle served to the runner.
    pub bundle: Bundle,
    /// The source and dependency roots to keep watching.
    pub watch_set: WatchSet,
}

/// Whatever hosts the app on the far end of a live session.
///
/// A desktop session spawns a runner binary beside this one; an exported Xcode
/// app builds and then launches itself. Both are, to the session, the same
/// thing: something that can be pointed at a server address, started, stopped,
/// and started again — which is all this trait asks, and why the watch loop has
/// no idea which kind it is driving.
pub(crate) trait LaunchedRunner {
    /// Brings a runner up against `bound`, ready to connect.
    fn start(&mut self, bound: std::net::SocketAddr) -> Result<(), LiveError>;

    /// Stops the runner, allowing `grace` for an orderly exit.
    fn stop(&mut self, grace: Duration) -> Result<(), LiveError>;

    /// How long a freshly started runner gets to make its connection.
    ///
    /// A spawned binary connects in milliseconds; a simulator boots an
    /// operating system first.
    fn connect_grace(&self) -> Duration {
        ACCEPT_GRACE
    }
}

/// The default window a runner has to connect within.
const ACCEPT_GRACE: Duration = Duration::from_secs(30);

/// A desktop runner process: spawned against the server's address and killed if
/// the supervisor unwinds.
///
/// A live session that fails must not leave an orphan runner holding the app's
/// window and the bundle's files. Killing on drop makes that structural.
struct DesktopRunner {
    /// Where this build's runner client lives; resolved once, at construction.
    path: std::path::PathBuf,
    /// The running child, from [`DesktopRunner::start`] until [`Self::stop`].
    child: Option<Child>,
}

impl DesktopRunner {
    fn new(runner: RunnerId) -> Result<Self, LiveError> {
        Ok(Self {
            path: crate::live::runner_client_path(runner)?,
            child: None,
        })
    }

    /// Waits for the runner to exit, killing it if it overstays `grace`.
    ///
    /// This is what `live.shutdown.finished` means: the runner is gone. Without
    /// it that event would be a `println!` next to another `println!`, printed
    /// while the runner was still running.
    fn shutdown(&mut self, grace: Duration) -> std::io::Result<()> {
        /// How often to re-check whether the runner has exited.
        const POLL: Duration = Duration::from_millis(5);

        let Some(child) = self.child.as_mut() else {
            return Ok(());
        };
        let deadline = Instant::now() + grace;
        loop {
            match child.try_wait()? {
                Some(_) => return Ok(()),
                None if Instant::now() >= deadline => {
                    child.kill()?;
                    child.wait()?;
                    return Ok(());
                }
                None => std::thread::sleep(POLL),
            }
        }
    }
}

impl LaunchedRunner for DesktopRunner {
    fn start(&mut self, bound: std::net::SocketAddr) -> Result<(), LiveError> {
        let child = Command::new(&self.path)
            .arg("--server")
            .arg(bound.to_string())
            .spawn()
            .map_err(|source| LiveError::Spawn {
                runner: "desktop",
                path: self.path.clone(),
                source,
            })?;
        self.child = Some(child);
        Ok(())
    }

    fn stop(&mut self, grace: Duration) -> Result<(), LiveError> {
        self.shutdown(grace)
            .map_err(|source| LiveError::Shutdown { source })
    }
}

impl Drop for DesktopRunner {
    fn drop(&mut self) {
        // Best effort: the process has usually exited through `stop` by now,
        // and a session that unwound before that is already reporting why.
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// Runs a live session, optionally watching for changes until it is time to quit.
pub(crate) fn run_desktop(options: &LiveOptions, rebuild: Rebuild<'_>) -> Result<(), LiveError> {
    run(
        options,
        rebuild,
        &mut DesktopRunner::new(options.runner)?,
        true,
    )
}

/// Runs a live session against an already-constructed launcher.
///
/// `headless` decides the bar a connection must clear before the session is
/// ready: a runner with no window stops at the entrypoint, and a windowed app
/// owes a presented frame.
pub(crate) fn run(
    options: &LiveOptions,
    rebuild: Rebuild<'_>,
    runner: &mut dyn LaunchedRunner,
    headless: bool,
) -> Result<(), LiveError> {
    let grace = runner.connect_grace();
    let initial = rebuild()?.ok_or(LiveError::NothingToRun)?;
    emit(&LiveEvent::BundleBuilt {
        payloads: initial.bundle.manifest().payloads.len(),
    });

    // Port 0: the OS picks. A fixed port would collide with a previous session
    // that has not finished dying.
    let address = std::net::SocketAddr::V4(std::net::SocketAddrV4::new(
        std::net::Ipv4Addr::LOCALHOST,
        0,
    ));
    let server = LiveServer::bind(address, initial.bundle.clone())?;
    let bound = server.local_addr()?;
    emit(&LiveEvent::ServerStarted {
        address: bound.to_string(),
    });

    runner.start(bound)?;
    let mut session =
        server.accept_session_within(initial.bundle, headless, grace, &mut |event| emit(&event))?;

    if options.watch {
        watch(
            options,
            initial.watch_set,
            &server,
            &mut session,
            runner,
            rebuild,
            headless,
        )?;
    } else {
        // An unwatched session is the app's, for as long as the app lasts. A
        // program that prints and returns ends it in milliseconds and an app
        // ends it when its window closes, and neither is a wait this end gets
        // to cut short — shutting down at `ready` would have killed the app on
        // its first frame.
        run_until_the_app_ends(&mut session, options.quit_after)?;
    }

    emit(&LiveEvent::ShutdownStarted);
    // The event below says the runner is gone, so this is where it goes. The
    // session is ended rather than dropped: the runner's goodbye is read, so it
    // leaves through the protocol instead of finding its socket gone.
    let _ = session.end(&mut |event| emit(&event));
    runner.stop(SHUTDOWN_GRACE)?;
    emit(&LiveEvent::ShutdownFinished);
    Ok(())
}

/// Keeps an unwatched session up until the app ends, or until the deadline.
///
/// With no deadline the wait is a blocking one, because there is nothing else
/// this session will ever do. With one it is polled, so that a `--quit-after`
/// bound holds over an app that would otherwise outlast it.
fn run_until_the_app_ends(
    session: &mut LiveSession,
    quit_after: Option<Duration>,
) -> Result<(), LiveError> {
    let Some(quit_after) = quit_after else {
        session.wait_for_app_exit(&mut |event| emit(&event))?;
        return Ok(());
    };

    let deadline = Instant::now() + quit_after;
    while Instant::now() < deadline {
        session.poll_runner(&mut |event| emit(&event))?;
        if session.app_exited() {
            return Ok(());
        }
        std::thread::sleep(RUNNER_POLL_INTERVAL);
    }
    Ok(())
}

/// Watches for changes and gets each one into the running app.
fn watch(
    options: &LiveOptions,
    watch_set: WatchSet,
    server: &LiveServer,
    session: &mut LiveSession,
    runner: &mut dyn LaunchedRunner,
    rebuild: Rebuild<'_>,
    headless: bool,
) -> Result<(), LiveError> {
    // The kill switch is read once, here, rather than per reload: a variable that
    // can change under a running session is a session that behaves two ways for
    // one invocation.
    let hotpatch_disabled = kira_live::hotpatch_disabled_by_env();
    // A watched session with no `--quit-after` ends when whoever started it ends
    // it, and nothing else — so it says so, once, rather than looking hung to
    // someone who expected the program to finish and exit.
    if options.runs_until_stopped() {
        eprintln!("kira: watching for changes; end the session with Ctrl-C");
    }
    let mut watcher = SourceWatcher::new(watch_set)?;
    // The baseline is now captured: from here on an edit will be seen. Announcing
    // it is what lets a tool — or a test — edit without racing the initial build.
    // A save that lands before this is folded into the baseline and lost, so the
    // signal has to come from after the snapshot, not from `live.session.ready`,
    // which is emitted before the watcher even exists.
    emit(&LiveEvent::Watching);
    let deadline = options.quit_after.map(|after| Instant::now() + after);

    loop {
        if let Some(deadline) = deadline
            && Instant::now() >= deadline
        {
            return Ok(());
        }

        // A watched session does not end when its app does: the runner is still
        // up, holding the cache and the loaded library, and the next save starts
        // the app again. The fact is reported where it happens rather than
        // discovered later, at whatever moment the session next reads.
        session.poll_runner(&mut |event| emit(&event))?;

        let wait = deadline
            .map(|deadline| deadline.saturating_duration_since(Instant::now()))
            .unwrap_or(RUNNER_POLL_INTERVAL);
        let changes = watcher.wait_for(wait)?;
        if changes.is_empty() {
            continue;
        }
        for change in &changes {
            emit(&LiveEvent::SourceChanged {
                path: change.path.display().to_string(),
            });
        }

        // A program that does not compile is not a reason to kill a running app.
        // Its diagnostics have already been printed by the rebuild.
        let Some(rebuilt) = rebuild()? else {
            continue;
        };
        watcher.update_set(rebuilt.watch_set)?;
        emit(&LiveEvent::BundleRebuilt);

        match session.reload(rebuilt.bundle, hotpatch_disabled, &mut |event| emit(&event))? {
            ReloadOutcome::Unchanged | ReloadOutcome::HotPatched => {}
            ReloadOutcome::NeedsRelaunch { .. } => {
                relaunch(server, session, runner, headless)?;
            }
        }
    }
}

/// Replaces the runner with a new one running the session's current bundle.
///
/// The session already announced why, so this does the work and says it
/// happened. The order matters: the old runner dies before the new one is
/// started, so two runners never hold the same bundle's files at once.
fn relaunch(
    server: &LiveServer,
    session: &mut LiveSession,
    runner: &mut dyn LaunchedRunner,
    headless: bool,
) -> Result<(), LiveError> {
    let bundle = session.bundle().clone();
    // Ask before killing. The runner is parked waiting for the next reload and
    // has no reason to exit on its own, so without this it sits out the whole
    // grace period and gets killed — turning every relaunch into a five-second
    // stall followed by a runner that never got to shut down cleanly.
    let _ = session.end(&mut |event| emit(&event));
    runner.stop(SHUTDOWN_GRACE)?;

    let bound = server.local_addr()?;
    runner.start(bound)?;
    *session =
        server.accept_session_within(bundle, headless, runner.connect_grace(), &mut |event| {
            emit(&event)
        })?;
    emit(&LiveEvent::RunnerRelaunched);
    Ok(())
}

/// Prints one event.
pub(crate) fn emit(event: &LiveEvent) {
    println!("{event}");
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A live session services the runner while the filesystem watcher blocks.
    #[test]
    fn the_runner_service_interval_is_not_a_busy_loop() {
        assert!(RUNNER_POLL_INTERVAL >= Duration::from_millis(50));
        assert!(
            RUNNER_POLL_INTERVAL <= Duration::from_millis(500),
            "a save should feel immediate"
        );
    }
}
