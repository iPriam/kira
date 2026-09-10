//! The channel a build reports its phases on.
//!
//! A build that prints nothing for two minutes is indistinguishable from one
//! that has hung, and the only person who can tell them apart is the one
//! holding a stopwatch. This is how each phase says it started.
//!
//! # Why a process-level sink
//!
//! Progress is not analysis. Nothing reads it back, nothing branches on it, and
//! a build that reports nothing produces byte-identical artifacts to one that
//! reports everything — so threading a reporter through every signature would
//! buy correctness that is already guaranteed and cost a parameter on every
//! function between the CLI and the linker. The sink is installed once, by the
//! binary that owns the terminal, and every layer below reports into it without
//! knowing whether anyone is listening.
//!
//! With no sink installed [`report`] does nothing, which is what a library
//! consumer and a test both want.

use std::sync::{Arc, RwLock};

/// Something that displays a build's phases.
pub trait ProgressSink: Send + Sync {
    /// Reports that `phase` has started.
    fn phase(&self, phase: &str);

    /// Refreshes the current line in place instead of adding one.
    ///
    /// For counters (`compiling shaders (3/12) …`): one line that keeps up
    /// rather than one line per item flooding the visible history. The
    /// default is a plain phase, so a sink that draws no in-place surface
    /// keeps working unchanged.
    fn update(&self, phase: &str) {
        self.phase(phase);
    }

    /// Takes the display down so something else can write.
    ///
    /// A surface that redraws in place and a diagnostic printed underneath it
    /// interleave into nonsense — half a status block, a note, then a status
    /// block that scrolled. Anything writing its own output says so first.
    fn suspend(&self);
}

/// Takes the installed display down for the life of the returned guard.
///
/// Nothing to restore: the surface redraws itself on the next phase, and a
/// build that reports no further phase had nothing left to show anyway.
pub fn suspended() -> Suspended {
    if let Ok(slot) = SINK.read()
        && let Some(sink) = slot.as_ref()
    {
        sink.suspend();
    }
    Suspended
}

/// The guard [`suspended`] returns.
#[derive(Debug)]
#[must_use = "the display stays down only while the guard is alive"]
pub struct Suspended;

/// The installed sink, or `None` when nobody is listening.
///
/// An `RwLock` rather than a `OnceLock`: a process runs more than one build —
/// a watched session rebuilds on every edit — and each needs to install its own
/// surface and take it down again.
static SINK: RwLock<Option<Arc<dyn ProgressSink>>> = RwLock::new(None);

/// Installs `sink` as the destination for every later [`report`].
pub fn install(sink: Arc<dyn ProgressSink>) {
    if let Ok(mut slot) = SINK.write() {
        *slot = Some(sink);
    }
}

/// Removes the installed sink, so later reports go nowhere.
pub fn uninstall() {
    if let Ok(mut slot) = SINK.write() {
        *slot = None;
    }
}

/// Reports that `phase` has started.
///
/// Two listeners, installed independently: the display, which draws it, and the
/// [`timeline`](crate::timeline), which times it. A build watched by neither
/// pays one uncontended read lock and a `None`. A poisoned lock is treated as no
/// listener rather than a panic — losing a progress line is never worth failing
/// a build over.
pub fn report(phase: &str) {
    crate::timeline::mark(phase);
    let Ok(slot) = SINK.read() else {
        return;
    };
    if let Some(sink) = slot.as_ref() {
        sink.phase(phase);
    }
}

/// Reports a live counter that refreshes the current display line.
///
/// Unlike [`report`], this never touches the [`timeline`](crate::timeline):
/// a counter's text names its item (`(3/12) Glass.ksl`), and timing that
/// would scatter one row per item across the report instead of one row for
/// the work. The caller opens the work with [`report`] first, so the timing
/// still attributes the whole run to that one phase.
pub fn report_live(phase: &str) {
    let Ok(slot) = SINK.read() else {
        return;
    };
    if let Some(sink) = slot.as_ref() {
        sink.update(phase);
    }
}

/// Reports a phase built from a format string, evaluating it only when someone
/// is listening.
///
/// A phase line often names a file or a count, and building that string for a
/// build nobody is watching is work with no reader.
#[macro_export]
macro_rules! progress {
    ($($arg:tt)*) => {
        if $crate::progress::watched() {
            $crate::progress::report(&format!($($arg)*));
        }
    };
}

/// Reports a live counter built from a format string, refreshing the current
/// display line instead of adding one. Gated like [`progress!`](crate::progress).
#[macro_export]
macro_rules! progress_live {
    ($($arg:tt)*) => {
        if $crate::progress::watched() {
            $crate::progress::report_live(&format!($($arg)*));
        }
    };
}

/// Whether a sink is installed.
#[must_use]
pub fn listening() -> bool {
    SINK.read().is_ok_and(|slot| slot.is_some())
}

/// Whether anything at all consumes phases — a display, a recording, or both.
///
/// What [`progress!`](crate::progress) gates its formatting on. Asking about
/// the display alone would leave `--timings` measuring only the phases whose
/// lines are constants, which is most of the cheap ones and none of the ones
/// that name a file.
#[must_use]
pub fn watched() -> bool {
    listening() || crate::timeline::recording()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, MutexGuard, OnceLock};

    /// Serializes the tests that install a sink.
    ///
    /// The sink is process-global, so two of these running at once trade
    /// installs and reports: one test's `uninstall` silences another's, and one
    /// test's phase lands in another's recorder. `cargo nextest` hides that by
    /// giving each test its own process; plain `cargo test` threads them and
    /// the race is real. Holding this for the duration is what makes both
    /// runners agree.
    fn exclusive() -> MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        // A test that panicked while holding the lock poisoned it, and the
        // global sink it left behind is exactly what `uninstall` below clears.
        // Refusing to run after an unrelated failure would turn one red test
        // into four.
        LOCK.get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// A sink that remembers what it was told.
    #[derive(Default)]
    struct Recorder {
        phases: Mutex<Vec<String>>,
        updates: Mutex<Vec<String>>,
    }

    impl ProgressSink for Recorder {
        fn phase(&self, phase: &str) {
            if let Ok(mut seen) = self.phases.lock() {
                seen.push(phase.to_owned());
            }
        }

        fn update(&self, phase: &str) {
            if let Ok(mut seen) = self.updates.lock() {
                seen.push(phase.to_owned());
            }
        }

        fn suspend(&self) {
            if let Ok(mut seen) = self.phases.lock() {
                seen.push("<suspended>".to_owned());
            }
        }
    }

    #[test]
    fn reporting_with_no_sink_installed_does_nothing() {
        let _exclusive = exclusive();
        uninstall();
        assert!(!listening());
        report("a phase nobody hears");
    }

    #[test]
    fn suspending_reaches_the_installed_sink() {
        let _exclusive = exclusive();
        let recorder = Arc::new(Recorder::default());
        install(recorder.clone());
        let _guard = suspended();
        uninstall();
        let seen = recorder.phases.lock().expect("the recorder");
        assert!(seen.contains(&"<suspended>".to_owned()), "{seen:?}");
    }

    #[test]
    fn suspending_with_no_sink_installed_does_nothing() {
        let _exclusive = exclusive();
        uninstall();
        let _guard = suspended();
    }

    #[test]
    fn an_installed_sink_receives_every_phase() {
        let _exclusive = exclusive();
        let recorder = Arc::new(Recorder::default());
        install(recorder.clone());
        report("parsing");
        report("linking");
        uninstall();
        let seen = recorder.phases.lock().expect("the recorder");
        assert!(seen.contains(&"parsing".to_owned()), "{seen:?}");
        assert!(seen.contains(&"linking".to_owned()), "{seen:?}");
    }

    #[test]
    fn a_live_counter_reaches_update_and_not_phase() {
        let _exclusive = exclusive();
        let recorder = Arc::new(Recorder::default());
        install(recorder.clone());
        report_live("compiling shaders (1/2) Glass.ksl");
        uninstall();
        let phases = recorder.phases.lock().expect("the recorder");
        let updates = recorder.updates.lock().expect("the recorder");
        assert!(phases.is_empty(), "{phases:?}");
        assert!(
            updates.contains(&"compiling shaders (1/2) Glass.ksl".to_owned()),
            "{updates:?}"
        );
    }

    #[test]
    fn a_live_counter_with_no_sink_installed_does_nothing() {
        let _exclusive = exclusive();
        uninstall();
        report_live("a counter nobody hears");
    }
}
