//! `kira profile report`: where a run spent its time, function by function.
//!
//! The columns are `perf report`'s, in `perf report`'s order, with `perf`'s
//! meaning: `Children` is the share of samples the function appeared anywhere
//! in, `Self` the share of samples it was the innermost frame of. The header
//! comments start with `#`, like `perf`'s, so a reader can skip them the same
//! way.

use std::collections::HashMap;
use std::fmt::Write as _;

use kira_core::Symbol;

use crate::model::{FrameId, FrameKind, Profile, share};
use crate::render::{Aggregate, ReportOptions, aggregate, command_column, percent};
use crate::trace::TraceMeta;

/// Renders the flat report, and the call graph when it was asked for.
#[must_use]
pub fn render(meta: &TraceMeta, profile: &Profile, options: &ReportOptions) -> String {
    let summary = aggregate(profile, options);
    let mut out = String::new();
    header(&mut out, meta, profile, &summary);
    table(
        &mut out,
        profile,
        &summary,
        options,
        &command_column(&meta.command),
    );
    if options.call_graph {
        out.push('\n');
        out.push_str(&call_graph(profile, options));
    }
    out
}

fn header(out: &mut String, meta: &TraceMeta, profile: &Profile, summary: &Aggregate) {
    let _ = writeln!(
        out,
        "# Recording: {} (backend {}, wall {}, exit {})",
        command_column(&meta.command),
        meta.backend.label(),
        meta.duration,
        meta.exit_code,
    );
    let _ = writeln!(
        out,
        "# View: {}   Event: {}   Collector: {}   Frequency: {} Hz",
        profile.view.label(),
        profile.event,
        profile.collector,
        profile.frequency,
    );
    let _ = writeln!(
        out,
        "# Samples: {}   Event count (approx.): {}   Lost: {}",
        summary.samples,
        profile.unit.render(summary.total),
        profile.lost,
    );
    out.push_str("#\n");
}

fn table(
    out: &mut String,
    profile: &Profile,
    summary: &Aggregate,
    options: &ReportOptions,
    command: &str,
) {
    if options.children {
        out.push_str("# Children      Self  Samples  Command   Shared Object    Symbol\n");
        out.push_str("# ........  ........  .......  ........  ...............  ......\n");
    } else {
        out.push_str("# Overhead  Samples  Command   Shared Object    Symbol\n");
        out.push_str("# ........  .......  ........  ...............  ......\n");
    }
    out.push_str("#\n");

    if summary.rows.is_empty() {
        out.push_str("# no samples\n");
        return;
    }

    let mut printed = 0;
    for row in &summary.rows {
        if printed >= options.limit {
            break;
        }
        let ranking = if options.children {
            row.children_weight
        } else {
            row.self_weight
        };
        if 100.0 * share(ranking, summary.total) < options.percent_limit {
            continue;
        }
        let frame = profile.frames.frame(row.frame);
        let symbol = symbol_column(profile, row.frame, options.per_instruction);
        let object = profile.frames.text(frame.object);
        if options.children {
            let _ = writeln!(
                out,
                "  {}  {}  {:>7}  {:<8}  {:<15}  {symbol}",
                percent(row.children_weight, summary.total),
                percent(row.self_weight, summary.total),
                row.samples,
                truncate(command, 8),
                truncate(object, 15),
            );
        } else {
            let _ = writeln!(
                out,
                "  {}  {:>7}  {:<8}  {:<15}  {symbol}",
                percent(row.self_weight, summary.total),
                row.samples,
                truncate(command, 8),
                truncate(object, 15),
            );
        }
        printed += 1;
    }
    if printed == 0 {
        let _ = writeln!(
            out,
            "# every row is below the {:.2}% threshold",
            options.percent_limit
        );
    }
}

/// The symbol column, with the kind marker `perf` puts in the same place.
///
/// The instruction offset is printed only when the rows are one instruction
/// each. A row that stands for a whole function has no single offset, and
/// printing the one its first frame happened to carry would name a line the
/// number beside it is not about.
#[must_use]
pub fn symbol_column(profile: &Profile, frame: FrameId, with_offset: bool) -> String {
    let entry = profile.frames.frame(frame);
    let name = profile.frames.text(entry.symbol);
    match entry.offset {
        Some(offset) if with_offset => format!("{} {name}+{offset}", entry.kind.marker()),
        _ => format!("{} {name}", entry.kind.marker()),
    }
}

fn truncate(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        return text.to_owned();
    }
    let kept = width.saturating_sub(2);
    let head = text.chars().take(kept).collect::<String>();
    format!("{head}..")
}

/// A node of the callee tree.
#[derive(Debug, Default)]
struct Node {
    weight: u64,
    children: HashMap<(Symbol, Symbol, FrameKind), usize>,
    order: Vec<usize>,
    frame: Option<FrameId>,
}

/// Renders the tree of what called what, heaviest branch first.
///
/// The tree is by callee: a node's children are the functions it called, which
/// is the direction a reader follows when the question is "what is this
/// function's time going into".
#[must_use]
pub fn call_graph(profile: &Profile, options: &ReportOptions) -> String {
    let mut nodes: Vec<Node> = vec![Node::default()];
    let mut total = 0u64;
    for sample in &profile.samples {
        if options.thread.is_some_and(|thread| thread != sample.thread) {
            continue;
        }
        total = total.saturating_add(sample.weight);
        let mut current = 0usize;
        add_weight(&mut nodes, current, sample.weight);
        for frame in &sample.stack {
            let entry = profile.frames.frame(*frame);
            let key = (entry.symbol, entry.object, entry.kind);
            let existing = nodes.get(current).and_then(|node| node.children.get(&key));
            let next = match existing {
                Some(index) => *index,
                None => {
                    let index = nodes.len();
                    nodes.push(Node {
                        frame: Some(*frame),
                        ..Node::default()
                    });
                    if let Some(node) = nodes.get_mut(current) {
                        node.children.insert(key, index);
                        node.order.push(index);
                    }
                    index
                }
            };
            add_weight(&mut nodes, next, sample.weight);
            current = next;
        }
    }

    let mut out = String::new();
    let _ = writeln!(
        out,
        "# Call graph (callees), below {:.2}% elided",
        options.percent_limit
    );
    out.push_str("#\n");
    write_node(&mut out, profile, &nodes, 0, total, 0, options);
    out
}

fn add_weight(nodes: &mut [Node], index: usize, weight: u64) {
    if let Some(node) = nodes.get_mut(index) {
        node.weight = node.weight.saturating_add(weight);
    }
}

fn write_node(
    out: &mut String,
    profile: &Profile,
    nodes: &[Node],
    index: usize,
    total: u64,
    depth: usize,
    options: &ReportOptions,
) {
    let Some(node) = nodes.get(index) else {
        return;
    };
    if let Some(frame) = node.frame {
        let _ = writeln!(
            out,
            "  {}{}  {}",
            "  ".repeat(depth.saturating_sub(1)),
            percent(node.weight, total),
            symbol_column(profile, frame, options.per_instruction),
        );
    }
    let mut children = node.order.clone();
    children
        .sort_by_key(|child| std::cmp::Reverse(nodes.get(*child).map_or(0, |node| node.weight)));
    for child in children {
        let weight = nodes.get(child).map_or(0, |node| node.weight);
        if 100.0 * share(weight, total) < options.percent_limit {
            continue;
        }
        write_node(out, profile, nodes, child, total, depth + 1, options);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Frame, Nanos, Sample, ThreadId, View};
    use crate::trace::TraceMeta;
    use kira_debug::Backend;

    fn meta() -> TraceMeta {
        TraceMeta {
            command: "hello".to_owned(),
            arguments: Vec::new(),
            backend: Backend::Vm,
            source: None,
            started_unix_ms: 0,
            duration: Nanos::new(2_000_000_000),
            exit_code: 0,
        }
    }

    fn profile() -> Profile {
        let mut profile = Profile::new(View::Kira, "kira-wall", "kira-runtime");
        profile.frequency = 997;
        let object = profile.frames.name("[vm]");
        let main_name = profile.frames.name("main");
        let step_name = profile.frames.name("Grid.step");
        let main = profile
            .frames
            .insert(Frame::named(main_name, FrameKind::Kira, object));
        let step = profile
            .frames
            .insert(Frame::named(step_name, FrameKind::Kira, object));
        profile.samples.push(Sample {
            thread: ThreadId::new(0),
            time: Nanos::new(0),
            weight: 20,
            stack: vec![main],
        });
        profile.samples.push(Sample {
            thread: ThreadId::new(0),
            time: Nanos::new(1),
            weight: 80,
            stack: vec![main, step],
        });
        profile
    }

    #[test]
    fn the_report_names_the_run_the_event_and_every_hot_function() {
        let text = render(&meta(), &profile(), &ReportOptions::default());
        assert!(text.contains("# Recording: hello (backend vm"), "{text}");
        assert!(text.contains("Event: kira-wall"), "{text}");
        assert!(text.contains("# Children      Self  Samples"), "{text}");
        assert!(text.contains("[K] Grid.step"), "{text}");
        assert!(text.contains("[K] main"), "{text}");
        // `main` is on every stack and is the leaf of one of them: all of the
        // children column, a fifth of the self column.
        let row = text
            .lines()
            .find(|line| line.ends_with("[K] main"))
            .expect("a row for main");
        assert!(row.contains("100.00%"), "{row}");
        assert!(row.contains("20.00%"), "{row}");
    }

    #[test]
    fn without_children_the_report_has_one_overhead_column() {
        let options = ReportOptions {
            children: false,
            ..ReportOptions::default()
        };
        let text = render(&meta(), &profile(), &options);
        assert!(text.contains("# Overhead  Samples"), "{text}");
        assert!(!text.contains("Children"), "{text}");
    }

    #[test]
    fn the_call_graph_nests_a_callee_under_its_caller() {
        let options = ReportOptions {
            call_graph: true,
            ..ReportOptions::default()
        };
        let text = render(&meta(), &profile(), &options);
        let main = text.find("  100.00%  [K] main").expect("main is the root");
        let step = text
            .find("    80.00%  [K] Grid.step")
            .expect("step under main");
        assert!(main < step, "{text}");
    }

    #[test]
    fn a_report_of_nothing_says_so_rather_than_printing_an_empty_table() {
        let empty = Profile::new(View::Machine, "cpu-clock", "none");
        let text = render(&meta(), &empty, &ReportOptions::default());
        assert!(text.contains("# no samples"), "{text}");
    }
}
