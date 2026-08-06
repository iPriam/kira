//! Where a build's time went.
//!
//! The measurement half of [`progress`](crate::progress). A phase line is
//! already reported at every boundary worth naming, so timing a build needs no
//! second set of call sites: the recorder listens to the same stream the status
//! surface draws, closes the previous phase when the next one opens, and adds
//! the durations up.
//!
//! # Why this is not the status surface
//!
//! The surface draws only when stderr is a terminal, and it draws the *last few*
//! phases — the two properties that make it useful to watch and useless to
//! measure. A timing report has to survive a pipe, has to keep every phase, and
//! has to be asked for. So it is a separate, independently installed listener
//! with the same feed.
//!
//! Phases do not nest. The stream is linear by construction — every reporter
//! names the work it is *about to start* — so a phase ends when the next begins,
//! and the last one ends when the recording is taken.

use std::sync::Mutex;
use std::time::{Duration, Instant};

/// One phase's share of a build.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recorded {
    /// The phase line as it was reported.
    pub name: String,
    /// How long it ran, summed over every time it was entered.
    pub elapsed: Duration,
    /// How many times it was entered.
    pub hits: usize,
}

/// A finished recording.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    /// Wall-clock time from [`start`] to [`take`].
    pub total: Duration,
    /// Every phase, longest first.
    pub phases: Vec<Recorded>,
    /// Time inside the recording that no phase claimed.
    ///
    /// Argument parsing before the first phase, and anything a build does
    /// between the last phase and the end. Kept rather than dropped: a report
    /// whose parts do not add up to its total is a report that hides the part
    /// nobody named.
    pub unattributed: Duration,
}

/// The recorder, or `None` when nobody asked for timings.
static RECORDER: Mutex<Option<Recorder>> = Mutex::new(None);

/// Accumulating state between [`start`] and [`take`].
struct Recorder {
    started: Instant,
    /// The phase currently running and when it began.
    open: Option<(String, Instant)>,
    /// Every phase entered so far, in first-seen order.
    phases: Vec<Recorded>,
}

impl Recorder {
    /// Closes the open phase at `now`, crediting it with its duration.
    fn close(&mut self, now: Instant) {
        let Some((name, began)) = self.open.take() else {
            return;
        };
        let elapsed = now.saturating_duration_since(began);
        match self.phases.iter_mut().find(|phase| phase.name == name) {
            Some(phase) => {
                phase.elapsed += elapsed;
                phase.hits += 1;
            }
            None => self.phases.push(Recorded {
                name,
                elapsed,
                hits: 1,
            }),
        }
    }
}

/// Begins recording, discarding anything a previous recording left.
///
/// A process runs more than one build — a watched session rebuilds on every
/// save — and each is timed on its own.
pub fn start() {
    if let Ok(mut slot) = RECORDER.lock() {
        *slot = Some(Recorder {
            started: Instant::now(),
            open: None,
            phases: Vec::new(),
        });
    }
}

/// Records that `phase` has started, ending whichever phase was running.
///
/// Called from [`progress::report`](crate::progress::report), so no reporter
/// knows this exists.
pub fn mark(phase: &str) {
    let now = Instant::now();
    let Ok(mut slot) = RECORDER.lock() else {
        return;
    };
    let Some(recorder) = slot.as_mut() else {
        return;
    };
    recorder.close(now);
    recorder.open = Some((phase.to_owned(), now));
}

/// Ends the recording and returns it, or `None` when none was running.
///
/// The phases come back longest first, which is the order the question "why is
/// this slow" is asked in.
#[must_use]
pub fn take() -> Option<Report> {
    let now = Instant::now();
    let mut recorder = RECORDER.lock().ok()?.take()?;
    recorder.close(now);
    let total = now.saturating_duration_since(recorder.started);
    let mut phases = recorder.phases;
    phases.sort_by(|left, right| {
        right
            .elapsed
            .cmp(&left.elapsed)
            .then_with(|| left.name.cmp(&right.name))
    });
    let claimed: Duration = phases.iter().map(|phase| phase.elapsed).sum();
    Some(Report {
        total,
        phases,
        unattributed: total.saturating_sub(claimed),
    })
}

/// Whether a recording is running.
#[must_use]
pub fn recording() -> bool {
    RECORDER.lock().is_ok_and(|slot| slot.is_some())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{MutexGuard, OnceLock};

    /// Serializes the tests, because the recorder is process-global: two
    /// running at once would record into each other's timeline.
    fn exclusive() -> MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    #[test]
    fn marking_with_no_recording_does_nothing() {
        let _exclusive = exclusive();
        let _ = take();
        assert!(!recording());
        mark("a phase nobody times");
        assert!(take().is_none());
    }

    #[test]
    fn every_phase_is_recorded_once_it_is_followed() {
        let _exclusive = exclusive();
        start();
        assert!(recording());
        mark("parsing");
        mark("linking");
        let report = take().expect("a recording");
        assert!(!recording());
        let names: Vec<&str> = report.phases.iter().map(|p| p.name.as_str()).collect();
        assert!(names.contains(&"parsing"), "{names:?}");
        // The last phase is closed by taking the report, so it is present too.
        assert!(names.contains(&"linking"), "{names:?}");
    }

    #[test]
    fn a_phase_entered_twice_is_summed_rather_than_listed_twice() {
        let _exclusive = exclusive();
        start();
        mark("emitting object");
        mark("linking");
        mark("emitting object");
        let report = take().expect("a recording");
        let emitting = report
            .phases
            .iter()
            .find(|phase| phase.name == "emitting object")
            .expect("the repeated phase");
        assert_eq!(emitting.hits, 2);
        assert_eq!(
            report.phases.len(),
            2,
            "one row per name: {:?}",
            report.phases
        );
    }

    #[test]
    fn the_parts_add_up_to_the_total() {
        let _exclusive = exclusive();
        start();
        mark("one");
        mark("two");
        let report = take().expect("a recording");
        let claimed: Duration = report.phases.iter().map(|phase| phase.elapsed).sum();
        assert_eq!(claimed + report.unattributed, report.total);
    }

    #[test]
    fn the_longest_phase_is_reported_first() {
        let _exclusive = exclusive();
        start();
        mark("quick");
        mark("slow");
        std::thread::sleep(Duration::from_millis(20));
        let report = take().expect("a recording");
        assert_eq!(report.phases[0].name, "slow");
    }

    #[test]
    fn a_second_recording_starts_from_nothing() {
        let _exclusive = exclusive();
        start();
        mark("first build");
        start();
        mark("second build");
        let report = take().expect("a recording");
        let names: Vec<&str> = report.phases.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, vec!["second build"]);
    }
}
