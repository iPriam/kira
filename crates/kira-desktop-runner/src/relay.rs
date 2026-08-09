//! Two threads: one runs the app, one talks the protocol. This is the seam.
//!
//! A Kira app is not a function that returns. It opens a window, and its run
//! loop owns the thread that started it until the window closes — which on macOS
//! must be the process's main thread, and on every platform must not be the
//! thread holding the live socket. A runner that started the app on the socket's
//! thread would never hear another word from the server, so `entrypoint started`
//! could only ever be reported by an app that had already exited.
//!
//! So the app keeps the main thread and the protocol moves off it. The protocol
//! thread drives a [`RelayHost`], which is a [`RunnerHost`] that does no work
//! itself: every call becomes a [`Work`] request, and the answer comes back from
//! the thread that owns the [`DesktopHost`]. Load, link, start, and swap all
//! happen on that one thread, in order, exactly as they would have with no
//! threads at all.
//!
//! The one thing that changes meaning is `start`. The app thread answers it
//! twice — once when the entrypoint is running, and again when it returns — so
//! the protocol thread can report the milestone without waiting for an app that
//! is never going to finish, and can still wait for the full run when it is
//! proving a swapped-in bundle works.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};

use kira_live::{AppOutcome, Bundle, RunnerHost};

use crate::host::DesktopHost;
use crate::hotpatch::VmHotPatch;

/// What the protocol thread asks the app thread to do.
enum Work {
    /// Stage the bundle and decode its entry.
    Load {
        /// The bundle to stage.
        bundle: Bundle,
        /// Answered when it is staged, or with why it is not.
        done: Reply,
    },
    /// Resolve what was loaded.
    Link {
        /// Answered when it is linked, or with why it is not.
        done: Reply,
    },
    /// Run the entrypoint.
    Start {
        /// Answered the moment the entrypoint is running, or with why it cannot.
        entered: Reply,
        /// Where the run's own outcome goes when somebody is waiting for it.
        ///
        /// A swap's proof waits; the session's own start does not, and for it
        /// this is `None` — the outcome is left in the exit slot instead, for
        /// the protocol thread to report whenever it next looks. Which of the
        /// two it is has to be said here rather than inferred from whether the
        /// receiver is still alive: a program that prints and returns can finish
        /// before the caller has stopped listening, and a runner that read that
        /// as "somebody is waiting" would swallow the exit it was meant to
        /// report.
        finished: Option<Reply>,
    },
    /// Swap a rebuilt bundle into the linked one.
    Swap {
        /// The rebuilt bundle.
        bundle: Bundle,
        /// Answered when the swap is committed, or with why it was refused.
        done: Reply,
    },
}

/// One answer from the app thread, carrying the host's own words on failure.
type Reply = Sender<Result<(), String>>;

/// Why a call the protocol thread made did not succeed.
#[derive(Debug, thiserror::Error)]
pub enum RelayError {
    /// The host refused, in its own words.
    #[error("{0}")]
    Host(String),
    /// The thread running the app is not answering.
    ///
    /// It has panicked or exited. Either way this runner is done: the app is
    /// gone, and no later request could be served.
    #[error("the thread hosting the app stopped answering")]
    AppThreadGone,
}

/// Where the app thread leaves an exit for the protocol thread to report.
///
/// A slot rather than a channel: the protocol thread polls it between looks at
/// the socket, and there is only ever one exit to hand over.
type ExitSlot = Arc<Mutex<Option<AppOutcome>>>;

/// The protocol thread's view of the host.
#[derive(Debug)]
pub struct RelayHost {
    work: Sender<Work>,
    running: Arc<AtomicBool>,
    exited: ExitSlot,
    hotpatch_disabled: bool,
    hotpatch: VmHotPatch,
    pending_generation: Option<u64>,
}

/// The app thread's end: the requests, and the app itself.
#[derive(Debug)]
pub struct AppThread {
    work: Receiver<Work>,
    running: Arc<AtomicBool>,
    exited: ExitSlot,
}

/// The two ends of one runner, split across the thread boundary.
///
/// `hotpatch_disabled` is passed in rather than read here so that both ends of a
/// session agree on it: the switch is read once, where the host is built.
pub fn pair(hotpatch_disabled: bool) -> (RelayHost, AppThread) {
    pair_with_hotpatch(hotpatch_disabled, VmHotPatch::new(std::path::PathBuf::new()))
}

/// Splits a runner while sharing its VM hot-patch controller with the protocol
/// thread. The controller is published by [`DesktopHost::link`] before the
/// entrypoint starts.
pub fn pair_with_hotpatch(
    hotpatch_disabled: bool,
    hotpatch: VmHotPatch,
) -> (RelayHost, AppThread) {
    let (work, requests) = channel();
    let running = Arc::new(AtomicBool::new(false));
    let exited: ExitSlot = Arc::new(Mutex::new(None));
    (
        RelayHost {
            work,
            running: Arc::clone(&running),
            exited: Arc::clone(&exited),
            hotpatch_disabled,
            hotpatch,
            pending_generation: None,
        },
        AppThread {
            work: requests,
            running,
            exited,
        },
    )
}

impl RelayHost {
    /// Sends `work` and waits for the answer on `answer`.
    fn ask(&self, work: Work, answer: &Receiver<Result<(), String>>) -> Result<(), RelayError> {
        self.work
            .send(work)
            .map_err(|_| RelayError::AppThreadGone)?;
        self.wait(answer)
    }

    /// Waits for one answer the app thread has already been asked for.
    fn wait(&self, answer: &Receiver<Result<(), String>>) -> Result<(), RelayError> {
        match answer.recv() {
            Ok(result) => result.map_err(RelayError::Host),
            Err(_) => Err(RelayError::AppThreadGone),
        }
    }
}

impl RunnerHost for RelayHost {
    type Error = RelayError;

    fn load(&mut self, bundle: &Bundle) -> Result<(), RelayError> {
        let (done, answer) = channel();
        self.ask(
            Work::Load {
                bundle: bundle.clone(),
                done,
            },
            &answer,
        )
    }

    fn link(&mut self) -> Result<(), RelayError> {
        let (done, answer) = channel();
        self.ask(Work::Link { done }, &answer)
    }

    fn start(&mut self) -> Result<(), RelayError> {
        let (entered, running) = channel();
        self.ask(
            Work::Start {
                entered,
                finished: None,
            },
            &running,
        )
    }

    fn run_once(&mut self) -> Result<(), RelayError> {
        // VM graphics reloads do not send a second Start request: their
        // original entrypoint remains suspended in the native window loop.
        // Waiting for a callback to enter the replacement is the proof the
        // live session reports as `reload.completed`.
        if let Some(generation) = self.pending_generation.take() {
            return self
                .hotpatch
                .wait_for_observation(generation)
                .then_some(())
                .ok_or_else(|| {
                    RelayError::Host(
                        "the swapped VM code was not observed by a live frame callback"
                            .to_owned(),
                    )
                });
        }
        let (entered, running) = channel();
        let (finished, returned) = channel();
        self.ask(
            Work::Start {
                entered,
                finished: Some(finished),
            },
            &running,
        )?;
        // Only ever asked after a swap, and a swap is only ever accepted by a
        // host that is idle — so this waits for a run that ends.
        self.wait(&returned)
    }

    fn swap(&mut self, bundle: &Bundle) -> Result<(), RelayError> {
        if self.hotpatch.has_active_vm() {
            let generation = self
                .hotpatch
                .swap(bundle)
                .map_err(|error| RelayError::Host(error.to_string()))?
                .ok_or_else(|| {
                    RelayError::Host("the VM hot-patch session disappeared".to_owned())
                })?;
            self.pending_generation = Some(generation);
            return Ok(());
        }
        let (done, answer) = channel();
        self.ask(
            Work::Swap {
                bundle: bundle.clone(),
                done,
            },
            &answer,
        )
    }

    fn take_app_exit(&mut self) -> Option<AppOutcome> {
        // A poisoned lock is the app thread having panicked while holding it,
        // which is a runner with no app. Taking the value anyway is the honest
        // read: whatever it says is still what the app thread last wrote.
        let mut exited = self.exited.lock().unwrap_or_else(|held| held.into_inner());
        exited.take()
    }

    fn hot_patch_refusal(&self) -> Option<String> {
        if self.hotpatch_disabled {
            return Some(kira_live::hotpatch_kill_switch_reason());
        }
        // VM-only graphics sessions switch the program used by future native
        // callbacks while preserving the current window. Mixed hybrid apps do
        // not have that guarantee: native code may still hold a stack into the
        // old image, so they retain the relaunch fallback.
        if self.hotpatch.has_active_vm() {
            return None;
        }
        self.running.load(Ordering::SeqCst).then(|| {
            "the app's entrypoint is still running, and a swap needs the runner idle".to_owned()
        })
    }
}

impl AppThread {
    /// The flag the protocol thread reads to know whether the app is running.
    pub fn running(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.running)
    }

    /// Leaves the app's outcome where the protocol thread will find it.
    fn record_exit(&self, outcome: Result<(), String>) {
        let outcome = match outcome {
            Ok(()) => AppOutcome::Finished,
            Err(reason) => AppOutcome::Failed(reason),
        };
        let mut slot = self.exited.lock().unwrap_or_else(|held| held.into_inner());
        *slot = Some(outcome);
    }

    /// Serves the protocol thread's requests until the session ends.
    ///
    /// Runs on whichever thread the app is allowed to own, which is the process's
    /// main thread.
    pub fn serve(self, host: &mut DesktopHost) {
        loop {
            // A closed channel is the protocol thread having finished: the
            // session ended and nothing else will be asked for.
            let Ok(work) = self.work.recv() else {
                return;
            };
            match work {
                Work::Load { bundle, done } => {
                    let _ = done.send(host.load(&bundle).map_err(|error| error.to_string()));
                }
                Work::Link { done } => {
                    let _ = done.send(host.link().map_err(|error| error.to_string()));
                }
                Work::Swap { bundle, done } => {
                    let _ = done.send(host.swap(&bundle).map_err(|error| error.to_string()));
                }
                Work::Start { entered, finished } => {
                    // Whether the entrypoint can start at all is knowable before
                    // running it, and it has to be: this is the last moment the
                    // protocol thread can be told that the app never started,
                    // because after the ack it is off reporting that it did.
                    if let Err(error) = host.startable() {
                        let _ = entered.send(Err(error.to_string()));
                        continue;
                    }
                    // The flag rises before the ack, so the protocol thread never
                    // sees a started session and an idle host — which would let a
                    // swap through under a running app.
                    self.running.store(true, Ordering::SeqCst);
                    let _ = entered.send(Ok(()));

                    let outcome = host.start().map_err(|error| error.to_string());
                    self.running.store(false, Ordering::SeqCst);

                    // The session's own start has nobody waiting: the app it
                    // started is over, and the protocol thread is the one that
                    // can say so. Left in the slot rather than acted on here,
                    // because whether an ended app ends the session is the
                    // server's decision, and this thread stays up either way.
                    match finished {
                        Some(reply) => {
                            let _ = reply.send(outcome);
                        }
                        None => self.record_exit(outcome),
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The rule a live app turns on: a swap needs the runner idle, and a run loop
    /// is the opposite of idle. Without this the protocol thread would offer a
    /// swap the app thread could not even hear, because it is inside the code the
    /// swap would replace.
    #[test]
    fn a_running_app_refuses_a_hot_patch() {
        let (relay, app) = pair(false);
        assert_eq!(relay.hot_patch_refusal(), None);

        app.running().store(true, Ordering::SeqCst);
        let refusal = relay.hot_patch_refusal().expect("a running app refuses");
        assert!(
            refusal.contains("still running"),
            "the refusal must say what is in the way, got `{refusal}`"
        );
    }

    /// The kill switch answers first, so a session run with the hot-patch path
    /// removed says that is why it relaunched rather than blaming the app.
    #[test]
    fn the_kill_switch_is_the_reason_it_gives() {
        let (relay, app) = pair(true);
        app.running().store(true, Ordering::SeqCst);
        assert_eq!(
            relay.hot_patch_refusal(),
            Some(kira_live::hotpatch_kill_switch_reason())
        );
    }

    /// A host whose thread is gone is a runner that cannot run anything. Reported
    /// as itself rather than as a host error, because no host said it.
    #[test]
    fn a_lost_app_thread_is_reported_as_itself() {
        let (mut relay, app) = pair(false);
        drop(app);
        let error = relay.link().expect_err("there is nobody to link");
        assert!(matches!(error, RelayError::AppThreadGone), "got {error:?}");
    }
}
