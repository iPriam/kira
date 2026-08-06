//! `--timings`: what the build spent its time on.
//!
//! The frontend already names every phase it enters, and
//! [`kira_diagnostics::timeline`] already turns that stream into durations.
//! This is the part that belongs to whoever owns the terminal: when to record,
//! and how the recording reads.
//!
//! On stderr, like the status surface and for the same reason — stdout carries
//! the build's result, and a timing report is not part of it.

use std::time::Duration;

use kira_diagnostics::timeline::{self, Report};

use crate::progress::err;

/// Records phase timings for as long as this is alive, then reports them.
///
/// Held by the verb handler, so the report is printed however the command
/// returns: the recording ends where the command does rather than where the
/// last phase happened to be.
pub struct Timings {
    /// Whether `--timings` asked for this. A handler constructs one either way
    /// so the guard is unconditional and the flag is read in exactly one place.
    enabled: bool,
}

impl Timings {
    /// Starts recording when `enabled`, and does nothing at all when not.
    pub fn install(enabled: bool) -> Self {
        if enabled {
            timeline::start();
        }
        Self { enabled }
    }
}

impl Drop for Timings {
    fn drop(&mut self) {
        if !self.enabled {
            return;
        }
        if let Some(report) = timeline::take() {
            print(&report);
        }
    }
}

/// The share of the build a phase took, as a percentage of the total.
///
/// A total of zero — a build fast enough to round to nothing — yields zero
/// rather than a division by it.
fn share(part: Duration, total: Duration) -> f64 {
    if total.is_zero() {
        return 0.0;
    }
    100.0 * part.as_secs_f64() / total.as_secs_f64()
}

/// Renders a report to stderr, longest phase first.
fn print(report: &Report) {
    let paint = kira_toolchain::Paint::auto_stderr();
    err!(
        "{} — total {:.3}s",
        paint.bold("timings"),
        report.total.as_secs_f64()
    );
    for phase in &report.phases {
        let repeats = if phase.hits > 1 {
            format!(" ×{}", phase.hits)
        } else {
            String::new()
        };
        err!(
            "  {:>8.3}s  {:>5.1}%  {}{}",
            phase.elapsed.as_secs_f64(),
            share(phase.elapsed, report.total),
            phase.name,
            paint.dim(&repeats),
        );
    }
    // Always printed, even at zero: a reader adding the rows up has to be able
    // to see that they add up.
    err!(
        "  {:>8.3}s  {:>5.1}%  {}",
        report.unattributed.as_secs_f64(),
        share(report.unattributed, report.total),
        paint.dim("(unattributed)"),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_phase_that_took_a_quarter_of_the_build_reads_as_a_quarter() {
        let quarter = share(Duration::from_millis(500), Duration::from_millis(2000));
        assert!((quarter - 25.0).abs() < f64::EPSILON, "{quarter}");
    }

    #[test]
    fn a_build_too_fast_to_measure_divides_by_nothing() {
        assert_eq!(share(Duration::ZERO, Duration::ZERO), 0.0);
    }

    #[test]
    fn nothing_is_recorded_unless_it_was_asked_for() {
        let _guard = Timings::install(false);
        assert!(!timeline::recording());
    }
}
