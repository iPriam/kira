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
//! # A build is not one thread
//!
//! A phase ends when the *same thread* opens the next one. That is what makes
//! this readable on a build that emits its codegen units in parallel: eight
//! threads each name the unit they are working on, and each unit is credited
//! with its own span rather than with whatever the last thread to speak was
//! doing.
//!
//! What a phase is credited with is **wall-clock**: the span from its first
//! start to its last end, with the gaps between repeats removed. Two phases that
//! ran at once therefore both claim the same seconds, and the rows can add up to
//! more than the total — which is the truth about a parallel build, and the
//! reason [`Report::concurrent`] says so rather than leaving a reader to work
//! out why the percentages exceed a hundred.

use std::collections::HashMap;
use std::sync::Mutex;
use std::thread::ThreadId;
use std::time::{Duration, Instant};

/// One phase's share of a build.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recorded {
    /// The phase line as it was reported.
    pub name: String,
    /// The wall-clock time this phase was running, on any thread.
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
    /// Whether two phases ever ran at the same time.
    ///
    /// True for any build that emits its codegen units in parallel. When it is,
    /// the rows overlap each other and sum to more than [`Report::total`].
    pub concurrent: bool,
}

/// The recorder, or `None` when nobody asked for timings.
static RECORDER: Mutex<Option<Recorder>> = Mutex::new(None);

/// One phase's run on one thread.
struct Span {
    name: String,
    began: Instant,
    ended: Instant,
}

/// Accumulating state between [`start`] and [`take`].
struct Recorder {
    started: Instant,
    /// The phase each thread is currently running, and when it began.
    open: HashMap<ThreadId, (String, Instant)>,
    /// Every phase run that has finished.
    spans: Vec<Span>,
}

impl Recorder {
    /// Closes `thread`'s open phase at `now`.
    fn close(&mut self, thread: ThreadId, now: Instant) {
        let Some((name, began)) = self.open.remove(&thread) else {
            return;
        };
        self.spans.push(Span {
            name,
            began,
            ended: now,
        });
    }

    /// Closes every thread's open phase at `now`.
    fn close_all(&mut self, now: Instant) {
        for (_thread, (name, began)) in self.open.drain() {
            self.spans.push(Span {
                name,
                began,
                ended: now,
            });
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
            open: HashMap::new(),
            spans: Vec::new(),
        });
    }
}

/// Records that `phase` has started, ending whichever phase this thread was
/// running.
///
/// Called from [`progress::report`](crate::progress::report), so no reporter
/// knows this exists.
pub fn mark(phase: &str) {
    let now = Instant::now();
    let thread = std::thread::current().id();
    let Ok(mut slot) = RECORDER.lock() else {
        return;
    };
    let Some(recorder) = slot.as_mut() else {
        return;
    };
    recorder.close(thread, now);
    recorder.open.insert(thread, (phase.to_owned(), now));
}

/// Ends the recording and returns it, or `None` when none was running.
///
/// The phases come back longest first, which is the order the question "why is
/// this slow" is asked in.
#[must_use]
pub fn take() -> Option<Report> {
    let now = Instant::now();
    let mut recorder = RECORDER.lock().ok()?.take()?;
    recorder.close_all(now);
    let total = now.saturating_duration_since(recorder.started);
    let spans = recorder.spans;

    // First seen first, so a report keeps the build's own order underneath the
    // sort by duration and two phases of equal length do not swap between runs.
    let mut order: Vec<&str> = Vec::new();
    let mut runs: HashMap<&str, Vec<(Instant, Instant)>> = HashMap::new();
    for span in &spans {
        let entry = runs.entry(span.name.as_str()).or_insert_with(|| {
            order.push(span.name.as_str());
            Vec::new()
        });
        entry.push((span.began, span.ended));
    }

    let mut phases: Vec<Recorded> = order
        .into_iter()
        .map(|name| {
            let intervals = &runs[name];
            Recorded {
                name: name.to_owned(),
                elapsed: covered(intervals.clone()),
                hits: intervals.len(),
            }
        })
        .collect();
    phases.sort_by(|left, right| {
        right
            .elapsed
            .cmp(&left.elapsed)
            .then_with(|| left.name.cmp(&right.name))
    });

    let claimed: Duration = phases.iter().map(|phase| phase.elapsed).sum();
    let everything: Vec<(Instant, Instant)> =
        spans.iter().map(|span| (span.began, span.ended)).collect();
    let covered_by_anything = covered(everything);
    Some(Report {
        total,
        phases,
        unattributed: total.saturating_sub(covered_by_anything),
        concurrent: claimed > covered_by_anything,
    })
}

/// How much wall-clock time a set of intervals covers, counting an instant that
/// two of them share only once.
fn covered(mut intervals: Vec<(Instant, Instant)>) -> Duration {
    intervals.sort_by_key(|&(began, _)| began);
    let mut total = Duration::ZERO;
    let mut open: Option<(Instant, Instant)> = None;
    for (began, ended) in intervals {
        match open {
            Some((start, end)) if began <= end => open = Some((start, end.max(ended))),
            Some((start, end)) => {
                total += end.saturating_duration_since(start);
                open = Some((began, ended));
            }
            None => open = Some((began, ended)),
        }
    }
    if let Some((start, end)) = open {
        total += end.saturating_duration_since(start);
    }
    total
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
        assert!(!report.concurrent);
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

    /// The property the parallel backend depends on: a phase is closed by the
    /// thread that opened it, so a worker's span is its own work and not the
    /// gap until some other worker happened to speak next.
    #[test]
    fn a_worker_is_credited_with_its_own_phase_and_not_another_thread_s() {
        let _exclusive = exclusive();
        start();
        mark("the main thread");
        std::thread::scope(|scope| {
            for unit in 0..4 {
                scope.spawn(move || {
                    mark(&format!("unit {unit}"));
                    std::thread::sleep(Duration::from_millis(40));
                });
            }
        });
        let report = take().expect("a recording");
        for unit in 0..4 {
            let recorded = report
                .phases
                .iter()
                .find(|phase| phase.name == format!("unit {unit}"))
                .expect("every worker's phase");
            assert!(
                recorded.elapsed >= Duration::from_millis(30),
                "{recorded:?}",
            );
        }
        // Four phases of 40ms inside a scope that took about 40ms: they
        // overlapped, and the report says so instead of hiding it.
        assert!(report.concurrent, "{report:?}");
        assert!(report.total < Duration::from_millis(160), "{report:?}");
    }

    /// A phase entered twice covers both runs, not the gap between them.
    #[test]
    fn a_repeated_phase_does_not_claim_the_time_between_its_runs() {
        let _exclusive = exclusive();
        start();
        mark("work");
        std::thread::sleep(Duration::from_millis(10));
        mark("idle");
        std::thread::sleep(Duration::from_millis(40));
        mark("work");
        std::thread::sleep(Duration::from_millis(10));
        let report = take().expect("a recording");
        let work = report
            .phases
            .iter()
            .find(|phase| phase.name == "work")
            .expect("the repeated phase");
        assert!(work.elapsed < Duration::from_millis(40), "{work:?}");
    }
}
