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
//! poll the watcher            -> nothing changed, almost always
//! rebuild                     -> the compiler decides if the edit is even valid
//! offer it to the session     -> which decides the tier and carries it out
//! relaunch if it must         -> and say so, loudly
//! ```
//!
//! A compile error does not end the session. An editor saves a file mid-thought
//! and the program does not parse; killing a running app over that would make
//! watching worse than not watching. The diagnostics print and the old app keeps
//! running, because the last bundle that built is still the best one there is.

use std::path::Path;
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use kira_live::{
    Bundle, LiveEvent, LiveServer, LiveSession, ReloadOutcome, SourceWatcher, WatchSet,
};
use kira_manifest::RunnerId;

use crate::live::{LiveError, LiveOptions};

/// How often the supervisor looks for a change.
///
/// Fast enough that a save feels immediate, slow enough that a session is not a
/// busy loop. A live session is a thing left running for hours in the background
/// of someone's editor; it does not get to burn a core.
const POLL_INTERVAL: Duration = Duration::from_millis(150);

/// How long a runner gets to exit on its own before the session kills it.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(5);

/// Builds the current source into a bundle.
///
/// `Ok(None)` means the program did not compile and its diagnostics have already
/// been reported — a session keeps running on the last bundle that did.
pub type Rebuild<'a> = &'a dyn Fn() -> Result<Option<Bundle>, LiveError>;

/// A runner child process that is killed if the supervisor unwinds.
///
/// A live session that fails must not leave an orphan runner holding the app's
/// window and the bundle's files. Killing on drop makes that structural.
struct RunnerProcess {
    child: Child,
}

impl RunnerProcess {
    /// Waits for the runner to exit, killing it if it overstays `grace`.
    ///
    /// This is what `live.shutdown.finished` means: the runner is gone. Without
    /// it that event would be a `println!` next to another `println!`, printed
    /// while the runner was still running.
    fn shutdown(&mut self, grace: Duration) -> std::io::Result<()> {
        /// How often to re-check whether the runner has exited.
        const POLL: Duration = Duration::from_millis(5);

        let deadline = Instant::now() + grace;
        loop {
            match self.child.try_wait()? {
                Some(_) => return Ok(()),
                None if Instant::now() >= deadline => {
                    self.child.kill()?;
                    self.child.wait()?;
                    return Ok(());
                }
                None => std::thread::sleep(POLL),
            }
        }
    }
}

impl Drop for RunnerProcess {
    fn drop(&mut self) {
        // Best effort: the process has usually exited through `shutdown` by now,
        // and a session that unwound before that is already reporting why.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Runs a live session, optionally watching for changes until it is time to quit.
pub fn run(options: &LiveOptions, source: &Path, rebuild: Rebuild<'_>) -> Result<(), LiveError> {
    if options.runner != RunnerId::Desktop {
        return Err(LiveError::NoRunnerClient {
            runner: options.runner.label(),
        });
    }

    let bundle = rebuild()?.ok_or(LiveError::NothingToRun)?;
    emit(&LiveEvent::BundleBuilt {
        payloads: bundle.manifest().payloads.len(),
    });

    // Port 0: the OS picks. A fixed port would collide with a previous session
    // that has not finished dying.
    let address = std::net::SocketAddr::V4(std::net::SocketAddrV4::new(
        std::net::Ipv4Addr::LOCALHOST,
        0,
    ));
    let server = LiveServer::bind(address, bundle.clone())?;
    let bound = server.local_addr()?;
    emit(&LiveEvent::ServerStarted {
        address: bound.to_string(),
    });

    let mut runner = spawn_runner(options.runner, bound)?;
    // Headless: this runner has no window to present to, so the session's bar is
    // the entrypoint. That is a real bar, not a lowered one — presenting a frame
    // needs a window and a swapchain that this repo does not own.
    let mut session = server.accept_session(bundle, true, &mut |event| emit(&event))?;

    if options.watch {
        watch(options, source, &server, &mut session, &mut runner, rebuild)?;
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
    runner
        .shutdown(SHUTDOWN_GRACE)
        .map_err(|source| LiveError::Shutdown { source })?;
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
        std::thread::sleep(POLL_INTERVAL);
    }
    Ok(())
}

/// Watches for changes and gets each one into the running app.
fn watch(
    options: &LiveOptions,
    source: &Path,
    server: &LiveServer,
    session: &mut LiveSession,
    runner: &mut RunnerProcess,
    rebuild: Rebuild<'_>,
) -> Result<(), LiveError> {
    // The kill switch is read once, here, rather than per reload: a variable that
    // can change under a running session is a session that behaves two ways for
    // one invocation.
    let hotpatch_disabled = kira_live::hotpatch_disabled_by_env();
    let mut watcher = SourceWatcher::new(watch_set(source));
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

        let changes = watcher.poll();
        if changes.is_empty() {
            std::thread::sleep(POLL_INTERVAL);
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
        emit(&LiveEvent::BundleRebuilt);

        match session.reload(rebuilt, hotpatch_disabled, &mut |event| emit(&event))? {
            ReloadOutcome::Unchanged | ReloadOutcome::HotPatched => {}
            ReloadOutcome::NeedsRelaunch { .. } => {
                relaunch(options, server, session, runner)?;
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
    options: &LiveOptions,
    server: &LiveServer,
    session: &mut LiveSession,
    runner: &mut RunnerProcess,
) -> Result<(), LiveError> {
    let bundle = session.bundle().clone();
    // Ask before killing. The runner is parked waiting for the next reload and
    // has no reason to exit on its own, so without this it sits out the whole
    // grace period and gets killed — turning every relaunch into a five-second
    // stall followed by a runner that never got to shut down cleanly.
    let _ = session.end(&mut |event| emit(&event));
    runner
        .shutdown(SHUTDOWN_GRACE)
        .map_err(|source| LiveError::Shutdown { source })?;

    let bound = server.local_addr()?;
    *runner = spawn_runner(options.runner, bound)?;
    *session = server.accept_session(bundle, true, &mut |event| emit(&event))?;
    emit(&LiveEvent::RunnerRelaunched);
    Ok(())
}

/// The inputs a change to which rebuilds this program.
///
/// Whatever the invocation named: one file for a standalone program, and the
/// package directory for a package — which the watcher walks, so a save
/// anywhere under `app/` reloads rather than only a save to the entry. The
/// watcher takes roots rather than a file precisely so that both are the same
/// watching.
fn watch_set(source: &Path) -> WatchSet {
    WatchSet::new().root(source)
}

/// Prints one event.
fn emit(event: &LiveEvent) {
    println!("{event}");
}

/// Starts the runner client for `runner`, pointed at `server`.
fn spawn_runner(
    runner: RunnerId,
    server: std::net::SocketAddr,
) -> Result<RunnerProcess, LiveError> {
    let path = crate::live::runner_client_path(runner)?;
    let child = Command::new(&path)
        .arg("--server")
        .arg(server.to_string())
        .spawn()
        .map_err(|source| LiveError::Spawn {
            runner: runner.label(),
            path,
            source,
        })?;
    Ok(RunnerProcess { child })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The watch set is what a session rebuilds from: the path the invocation
    /// named, file or package directory alike.
    #[test]
    fn the_watch_set_is_the_program() {
        let set = watch_set(Path::new("/tmp/app.kira"));
        assert_eq!(set.roots(), [std::path::PathBuf::from("/tmp/app.kira")]);
        let package = watch_set(Path::new("/tmp/liquid-glass-app"));
        assert_eq!(
            package.roots(),
            [std::path::PathBuf::from("/tmp/liquid-glass-app")]
        );
    }

    /// A live session polls in the background of somebody's editor for hours. It
    /// must not spin.
    #[test]
    fn the_poll_interval_is_not_a_busy_loop() {
        assert!(POLL_INTERVAL >= Duration::from_millis(50));
        assert!(
            POLL_INTERVAL <= Duration::from_millis(500),
            "a save should feel immediate"
        );
    }
}
