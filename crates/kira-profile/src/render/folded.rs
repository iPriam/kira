//! `kira profile report --folded`: collapsed stacks, one per line.
//!
//! The de facto interchange format for flame graphs: a semicolon-separated
//! stack from the outermost frame inwards, a space, and the weight. Every
//! flame-graph renderer in existence reads it, so a Kira profile can be looked
//! at in one without Kira shipping a renderer of its own.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use crate::model::Profile;
use crate::render::ReportOptions;

/// Renders collapsed stacks, heaviest last so the output is stable.
#[must_use]
pub fn render(profile: &Profile, options: &ReportOptions) -> String {
    let mut folded: BTreeMap<String, u64> = BTreeMap::new();
    for sample in &profile.samples {
        if options.thread.is_some_and(|thread| thread != sample.thread) {
            continue;
        }
        let stack = sample
            .stack
            .iter()
            .map(|frame| profile.frames.symbol_of(*frame).replace(';', ":"))
            .collect::<Vec<_>>()
            .join(";");
        let entry = folded.entry(stack).or_default();
        *entry = entry.saturating_add(sample.weight);
    }
    let mut out = String::new();
    for (stack, weight) in folded {
        let _ = writeln!(out, "{stack} {weight}");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Frame, FrameKind, Nanos, Sample, ThreadId, View};

    #[test]
    fn identical_stacks_fold_into_one_line() {
        let mut profile = Profile::new(View::Kira, "kira-wall", "test");
        let object = profile.frames.name("[vm]");
        let name = profile.frames.name("main");
        let frame = profile
            .frames
            .insert(Frame::named(name, FrameKind::Kira, object));
        for _ in 0..2 {
            profile.samples.push(Sample {
                thread: ThreadId::new(0),
                time: Nanos::ZERO,
                weight: 5,
                stack: vec![frame],
            });
        }
        assert_eq!(render(&profile, &ReportOptions::default()), "main 10\n");
    }
}
