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
/// Cheap and total when nothing is installed: one uncontended read lock and a
/// `None`. A poisoned lock is treated as no sink rather than a panic — losing a
/// progress line is never worth failing a build over.
pub fn report(phase: &str) {
    let Ok(slot) = SINK.read() else {
        return;
    };
    if let Some(sink) = slot.as_ref() {
        sink.phase(phase);
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
        if $crate::progress::listening() {
            $crate::progress::report(&format!($($arg)*));
        }
    };
}

/// Whether a sink is installed.
#[must_use]
pub fn listening() -> bool {
    SINK.read().is_ok_and(|slot| slot.is_some())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// A sink that remembers what it was told.
    #[derive(Default)]
    struct Recorder(Mutex<Vec<String>>);

    impl ProgressSink for Recorder {
        fn phase(&self, phase: &str) {
            if let Ok(mut seen) = self.0.lock() {
                seen.push(phase.to_owned());
            }
        }

        fn suspend(&self) {
            if let Ok(mut seen) = self.0.lock() {
                seen.push("<suspended>".to_owned());
            }
        }
    }

    #[test]
    fn reporting_with_no_sink_installed_does_nothing() {
        uninstall();
        assert!(!listening());
        report("a phase nobody hears");
    }

    #[test]
    fn suspending_reaches_the_installed_sink() {
        let recorder = Arc::new(Recorder::default());
        install(recorder.clone());
        let _guard = suspended();
        uninstall();
        let seen = recorder.0.lock().expect("the recorder");
        assert!(seen.contains(&"<suspended>".to_owned()), "{seen:?}");
    }

    #[test]
    fn suspending_with_no_sink_installed_does_nothing() {
        uninstall();
        let _guard = suspended();
    }

    #[test]
    fn an_installed_sink_receives_every_phase() {
        let recorder = Arc::new(Recorder::default());
        install(recorder.clone());
        report("parsing");
        report("linking");
        uninstall();
        let seen = recorder.0.lock().expect("the recorder");
        assert!(seen.contains(&"parsing".to_owned()), "{seen:?}");
        assert!(seen.contains(&"linking".to_owned()), "{seen:?}");
    }
}
