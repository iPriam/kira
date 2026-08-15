//! `kira profile script`: every sample, one stack at a time.
//!
//! The raw view, in the shape `perf script` prints: a header line naming the
//! command, thread, time, weight, and event, then one indented line per frame
//! from the innermost outwards. Anything that can consume `perf script` output
//! — a flame-graph collapser, a diffing script, an agent reading stacks — can
//! consume this.

use std::fmt::Write as _;

use crate::model::Profile;
use crate::render::{ReportOptions, command_column};
use crate::trace::TraceMeta;

/// Renders every sample as a stack.
#[must_use]
pub fn render(meta: &TraceMeta, profile: &Profile, options: &ReportOptions) -> String {
    let command = command_column(&meta.command);
    let mut out = String::new();
    for sample in &profile.samples {
        if options.thread.is_some_and(|thread| thread != sample.thread) {
            continue;
        }
        let seconds = sample.time.get() as f64 / 1e9;
        let _ = writeln!(
            out,
            "{command} {} {seconds:.9}: {} {}:",
            profile.thread_name(sample.thread),
            sample.weight,
            profile.event,
        );
        for frame in sample.stack.iter().rev() {
            let entry = profile.frames.frame(*frame);
            let name = profile.frames.text(entry.symbol);
            let object = profile.frames.text(entry.object);
            match entry.offset {
                Some(offset) => {
                    let _ = writeln!(out, "\t{name}+{offset} ({object})");
                }
                None => {
                    let _ = writeln!(out, "\t{name} ({object})");
                }
            }
        }
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Frame, FrameKind, Nanos, Sample, ThreadId, ThreadRecord, View};
    use kira_debug::Backend;

    #[test]
    fn a_sample_prints_its_stack_innermost_first() {
        let mut profile = Profile::new(View::Kira, "kira-wall", "kira-runtime");
        profile.threads.push(ThreadRecord {
            id: ThreadId::new(0),
            name: "main".to_owned(),
        });
        let object = profile.frames.name("[vm]");
        let outer = profile.frames.name("main");
        let inner = profile.frames.name("Grid.step");
        let outer = profile
            .frames
            .insert(Frame::named(outer, FrameKind::Kira, object));
        let inner = profile.frames.insert(Frame {
            offset: Some(12),
            ..Frame::named(inner, FrameKind::Kira, object)
        });
        profile.samples.push(Sample {
            thread: ThreadId::new(0),
            time: Nanos::new(1_500_000_000),
            weight: 1_000_000,
            stack: vec![outer, inner],
        });

        let meta = TraceMeta {
            command: "hello".to_owned(),
            arguments: Vec::new(),
            backend: Backend::Vm,
            source: None,
            started_unix_ms: 0,
            duration: Nanos::new(0),
            exit_code: 0,
        };
        let text = render(&meta, &profile, &ReportOptions::default());
        let lines = text.lines().collect::<Vec<_>>();
        assert_eq!(lines[0], "hello main 1.500000000: 1000000 kira-wall:");
        assert_eq!(lines[1], "\tGrid.step+12 ([vm])");
        assert_eq!(lines[2], "\tmain ([vm])");
    }
}
