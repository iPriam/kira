//! `kira profile diff`: what changed between two recordings.
//!
//! The verb that makes a profiler useful for optimisation rather than for
//! curiosity. Two recordings of the same program are joined by symbol and the
//! shares are subtracted, exactly as `perf diff` does, so a change shows up as
//! a signed percentage next to the function it moved.
//!
//! Shares rather than absolute weights, because two runs are never the same
//! length: a function that took the same time in a run that got twice as fast
//! doubled its share of it, and that is the thing worth seeing.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use crate::model::{Profile, share};
use crate::render::{ReportOptions, aggregate};

/// One symbol, and how its share moved.
#[derive(Debug, Clone, PartialEq)]
struct Change {
    symbol: String,
    marker: &'static str,
    baseline: f64,
    current: f64,
    baseline_weight: u64,
    current_weight: u64,
}

impl Change {
    fn delta(&self) -> f64 {
        self.current - self.baseline
    }
}

/// Renders the difference between `baseline` and `current`.
#[must_use]
pub fn render(baseline: &Profile, current: &Profile, options: &ReportOptions) -> String {
    let mut changes: BTreeMap<String, Change> = BTreeMap::new();
    collect(baseline, options, &mut changes, true);
    collect(current, options, &mut changes, false);

    let mut rows = changes.into_values().collect::<Vec<_>>();
    rows.sort_by(|left, right| {
        right
            .delta()
            .abs()
            .partial_cmp(&left.delta().abs())
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let mut out = String::new();
    let _ = writeln!(
        out,
        "# Baseline: {} ({} samples)   Current: {} ({} samples)",
        baseline.event,
        baseline.samples.len(),
        current.event,
        current.samples.len(),
    );
    out.push_str("#\n");
    out.push_str("# Baseline   Current     Delta  Symbol\n");
    out.push_str("# ........  ........  ........  ......\n");
    out.push_str("#\n");
    let mut printed = 0;
    for change in rows {
        if printed >= options.limit {
            break;
        }
        if change.delta().abs() * 100.0 < options.percent_limit
            && change.baseline_weight == 0
            && change.current_weight == 0
        {
            continue;
        }
        let _ = writeln!(
            out,
            "  {:>7.2}%  {:>7.2}%  {:>+7.2}%  {} {}",
            100.0 * change.baseline,
            100.0 * change.current,
            100.0 * change.delta(),
            change.marker,
            change.symbol,
        );
        printed += 1;
    }
    if printed == 0 {
        out.push_str("# the two recordings agree everywhere\n");
    }
    out
}

fn collect(
    profile: &Profile,
    options: &ReportOptions,
    changes: &mut BTreeMap<String, Change>,
    is_baseline: bool,
) {
    let summary = aggregate(profile, options);
    for row in &summary.rows {
        let frame = profile.frames.frame(row.frame);
        let symbol = profile.frames.text(frame.symbol).to_owned();
        let fraction = share(row.self_weight, summary.total);
        let change = changes.entry(symbol.clone()).or_insert(Change {
            symbol,
            marker: frame.kind.marker(),
            baseline: 0.0,
            current: 0.0,
            baseline_weight: 0,
            current_weight: 0,
        });
        if is_baseline {
            change.baseline = fraction;
            change.baseline_weight = row.self_weight;
        } else {
            change.current = fraction;
            change.current_weight = row.self_weight;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Frame, FrameKind, Nanos, Sample, ThreadId, View};

    fn profile(step: u64, other: u64) -> Profile {
        let mut profile = Profile::new(View::Kira, "kira-wall", "test");
        let object = profile.frames.name("[vm]");
        for (name, weight) in [("Grid.step", step), ("Grid.draw", other)] {
            let symbol = profile.frames.name(name);
            let frame = profile
                .frames
                .insert(Frame::named(symbol, FrameKind::Kira, object));
            profile.samples.push(Sample {
                thread: ThreadId::new(0),
                time: Nanos::ZERO,
                weight,
                stack: vec![frame],
            });
        }
        profile
    }

    #[test]
    fn a_function_that_lost_share_shows_a_negative_delta() {
        let text = render(
            &profile(80, 20),
            &profile(40, 60),
            &ReportOptions::default(),
        );
        let step = text
            .lines()
            .find(|line| line.contains("Grid.step"))
            .expect("a row for the function that changed");
        assert!(step.contains("80.00%"), "{step}");
        assert!(step.contains("40.00%"), "{step}");
        assert!(step.contains("-40.00%"), "{step}");
    }

    #[test]
    fn two_identical_recordings_report_no_movement() {
        let text = render(
            &profile(50, 50),
            &profile(50, 50),
            &ReportOptions::default(),
        );
        for line in text.lines().filter(|line| !line.starts_with('#')) {
            assert!(line.contains("+0.00%"), "{line}");
        }
    }
}
