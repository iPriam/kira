//! `kira profile stat`: the one-screen summary of a run.
//!
//! `perf stat` answers "how long, how much, how many" before anyone opens a
//! report. So does this — with the two numbers a Kira reader wants first: how
//! much of the run was the program's own code, and how much was the runtime
//! underneath it.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use crate::model::{FrameKind, Profile, View, share};
use crate::render::{ReportOptions, aggregate, command_column};
use crate::trace::Trace;

/// Renders the summary of a whole recording.
#[must_use]
pub fn render(trace: &Trace) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        " Profile of '{}'{}:",
        command_column(&trace.meta.command),
        arguments(&trace.meta.arguments),
    );
    out.push('\n');
    let _ = writeln!(
        out,
        "   {:>14}   backend, exit code {}",
        trace.meta.backend.label(),
        trace.meta.exit_code
    );
    let _ = writeln!(
        out,
        "   {:>14}   wall time",
        trace.meta.duration.to_string()
    );

    for profile in &trace.profiles {
        let total = profile.total();
        let _ = writeln!(
            out,
            "   {:>14}   {} ({} view, {} samples, {} lost, {} Hz)",
            profile.unit.render(total),
            profile.event,
            profile.view.label(),
            profile.samples.len(),
            profile.lost,
            profile.frequency,
        );
    }

    if let Some(kira) = trace.view(View::Kira) {
        out.push('\n');
        out.push_str(" Kira functions by self time:\n");
        top_functions(&mut out, kira);
    }
    if let Some(machine) = trace.view(View::Machine)
        && !machine.samples.is_empty()
    {
        out.push('\n');
        out.push_str(" Machine time by kind:\n");
        by_kind(&mut out, machine);
    }
    out.push('\n');
    let _ = writeln!(
        out,
        "   {:>14.6} seconds time elapsed",
        trace.meta.duration.get() as f64 / 1e9
    );
    out
}

fn arguments(arguments: &[String]) -> String {
    if arguments.is_empty() {
        return String::new();
    }
    format!(" {}", arguments.join(" "))
}

fn top_functions(out: &mut String, profile: &Profile) {
    let options = ReportOptions {
        limit: 5,
        ..ReportOptions::default()
    };
    let summary = aggregate(profile, &options);
    if summary.rows.is_empty() {
        out.push_str("   (none)\n");
        return;
    }
    for row in summary.rows.iter().take(options.limit) {
        if row.self_weight == 0 {
            continue;
        }
        let _ = writeln!(
            out,
            "   {:>6.2}%   {:>12}   {}",
            100.0 * share(row.self_weight, summary.total),
            profile.unit.render(row.self_weight),
            profile.frames.symbol_of(row.frame),
        );
    }
}

fn by_kind(out: &mut String, profile: &Profile) {
    let mut totals: BTreeMap<&'static str, u64> = BTreeMap::new();
    let mut total = 0u64;
    for sample in &profile.samples {
        let kind = sample
            .leaf()
            .map(|leaf| profile.frames.frame(leaf).kind)
            .unwrap_or(FrameKind::Unknown);
        total = total.saturating_add(sample.weight);
        let entry = totals.entry(kind.label()).or_default();
        *entry = entry.saturating_add(sample.weight);
    }
    let mut rows = totals.into_iter().collect::<Vec<_>>();
    rows.sort_by_key(|(_, weight)| std::cmp::Reverse(*weight));
    for (kind, weight) in rows {
        let _ = writeln!(
            out,
            "   {:>6.2}%   {:>12}   {kind}",
            100.0 * share(weight, total),
            profile.unit.render(weight),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Frame, Nanos, Sample, ThreadId};
    use crate::trace::TraceMeta;
    use kira_debug::Backend;

    fn trace() -> Trace {
        let mut kira = Profile::new(View::Kira, "kira-wall", "kira-runtime");
        kira.frequency = 997;
        let object = kira.frames.name("[vm]");
        let name = kira.frames.name("Grid.step");
        let frame = kira
            .frames
            .insert(Frame::named(name, FrameKind::Kira, object));
        kira.samples.push(Sample {
            thread: ThreadId::new(0),
            time: Nanos::ZERO,
            weight: 1_000_000,
            stack: vec![frame],
        });

        let mut machine = Profile::new(View::Machine, "cpu-clock", "windows-dbghelp");
        let image = machine.frames.name("kira.exe");
        let symbol = machine.frames.name("kira_vm_runtime::interp::Vm::step");
        let frame = machine
            .frames
            .insert(Frame::named(symbol, FrameKind::Runtime, image));
        machine.samples.push(Sample {
            thread: ThreadId::new(0),
            time: Nanos::ZERO,
            weight: 900_000,
            stack: vec![frame],
        });

        Trace {
            meta: TraceMeta {
                command: "hello".to_owned(),
                arguments: vec!["--rows".to_owned()],
                backend: Backend::Vm,
                source: None,
                started_unix_ms: 0,
                duration: Nanos::new(2_000_000_000),
                exit_code: 0,
            },
            profiles: vec![kira, machine],
        }
    }

    #[test]
    fn the_summary_names_the_run_every_view_and_the_hottest_function() {
        let text = render(&trace());
        assert!(text.contains("Profile of 'hello' --rows"), "{text}");
        assert!(text.contains("kira-wall (kira view"), "{text}");
        assert!(text.contains("cpu-clock (machine view"), "{text}");
        assert!(text.contains("Grid.step"), "{text}");
        assert!(text.contains("runtime"), "{text}");
        assert!(text.contains("2.000000 seconds time elapsed"), "{text}");
    }
}
