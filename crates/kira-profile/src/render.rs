//! Turning a recording into the reports a reader asks for.
//!
//! Every renderer here writes plain text with no colour and no cursor control.
//! That is deliberate: these reports are read as often by a program as by a
//! person, and `perf`'s output is valuable precisely because it is stable
//! enough to parse.
//!
//! The aggregation the reports share lives here — how a stack's weight is
//! divided between the function it ended in and the functions that called it.

use std::collections::HashMap;

use kira_core::Symbol;

use crate::model::{FrameId, FrameKind, Profile, ThreadId, share};

pub mod annotate;
pub mod diff;
pub mod folded;
pub mod report;
pub mod script;
pub mod stat;

/// What a report is ordered by.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sort {
    /// Time in the function itself.
    SelfTime,
    /// Time in the function and everything it called.
    Children,
    /// The symbol name.
    Symbol,
    /// The image the symbol came from.
    Object,
}

impl Sort {
    /// The order a command-line word names.
    #[must_use]
    pub fn parse(word: &str) -> Option<Self> {
        match word {
            "self" | "overhead" => Some(Self::SelfTime),
            "children" => Some(Self::Children),
            "symbol" => Some(Self::Symbol),
            "object" | "dso" => Some(Self::Object),
            _ => None,
        }
    }
}

/// What a report shows and how much of it.
#[derive(Debug, Clone)]
pub struct ReportOptions {
    /// Show a `Children` column holding time spent in everything a function
    /// called, as `perf report --children` does.
    pub children: bool,
    /// Print the call graph under each row.
    pub call_graph: bool,
    /// Rows with a smaller share than this are not printed.
    pub percent_limit: f64,
    /// The most rows to print.
    pub limit: usize,
    /// What to order by.
    pub sort: Sort,
    /// Only samples from this thread.
    pub thread: Option<ThreadId>,
    /// Only rows whose symbol contains this text.
    pub symbol: Option<String>,
    /// Group by instruction rather than by function.
    pub per_instruction: bool,
}

impl Default for ReportOptions {
    fn default() -> Self {
        Self {
            children: true,
            call_graph: false,
            percent_limit: 0.05,
            limit: 40,
            sort: Sort::SelfTime,
            thread: None,
            symbol: None,
            per_instruction: false,
        }
    }
}

/// One row of a flat report: a function, and what the run spent in it.
#[derive(Debug, Clone)]
pub struct Row {
    /// A frame with this row's identity, for its name, kind, and image.
    pub frame: FrameId,
    /// Weight of samples whose innermost frame was this one.
    pub self_weight: u64,
    /// Weight of samples this frame appeared anywhere in.
    pub children_weight: u64,
    /// How many samples ended in this frame.
    pub samples: u64,
}

/// How a profile's weight divides between its functions.
#[derive(Debug)]
pub struct Aggregate {
    /// Every row, in the order the options asked for.
    pub rows: Vec<Row>,
    /// The weight of every sample counted.
    pub total: u64,
    /// How many samples were counted.
    pub samples: u64,
}

/// The identity two frames share when they are the same row.
type RowKey = (Symbol, Symbol, FrameKind, Option<u32>);

/// Divides `profile`'s weight between its functions.
#[must_use]
pub fn aggregate(profile: &Profile, options: &ReportOptions) -> Aggregate {
    let mut index: HashMap<RowKey, usize> = HashMap::new();
    let mut rows: Vec<Row> = Vec::new();
    let mut total = 0u64;
    let mut counted = 0u64;
    let mut seen: Vec<usize> = Vec::new();

    for sample in &profile.samples {
        if options.thread.is_some_and(|thread| thread != sample.thread) {
            continue;
        }
        total = total.saturating_add(sample.weight);
        counted += 1;
        seen.clear();
        for (position, frame) in sample.stack.iter().enumerate() {
            let row = row_of(profile, options, &mut index, &mut rows, *frame);
            let innermost = position + 1 == sample.stack.len();
            if innermost && let Some(entry) = rows.get_mut(row) {
                entry.self_weight = entry.self_weight.saturating_add(sample.weight);
                entry.samples += 1;
            }
            // A function that calls itself is still one function: its
            // cumulative weight counts each sample once, which is what keeps a
            // recursive program's percentages inside a hundred.
            if !seen.contains(&row) {
                seen.push(row);
                if let Some(entry) = rows.get_mut(row) {
                    entry.children_weight = entry.children_weight.saturating_add(sample.weight);
                }
            }
        }
    }

    if let Some(filter) = &options.symbol {
        rows.retain(|row| {
            profile
                .frames
                .symbol_of(row.frame)
                .contains(filter.as_str())
        });
    }
    sort_rows(profile, &mut rows, options.sort);
    Aggregate {
        rows,
        total,
        samples: counted,
    }
}

fn row_of(
    profile: &Profile,
    options: &ReportOptions,
    index: &mut HashMap<RowKey, usize>,
    rows: &mut Vec<Row>,
    frame: FrameId,
) -> usize {
    let entry = profile.frames.frame(frame);
    let key = (
        entry.symbol,
        entry.object,
        entry.kind,
        options.per_instruction.then_some(entry.offset).flatten(),
    );
    match index.get(&key) {
        Some(row) => *row,
        None => {
            let row = rows.len();
            rows.push(Row {
                frame,
                self_weight: 0,
                children_weight: 0,
                samples: 0,
            });
            index.insert(key, row);
            row
        }
    }
}

fn sort_rows(profile: &Profile, rows: &mut [Row], sort: Sort) {
    match sort {
        Sort::SelfTime => rows.sort_by(|left, right| {
            right
                .self_weight
                .cmp(&left.self_weight)
                .then_with(|| right.children_weight.cmp(&left.children_weight))
        }),
        Sort::Children => rows.sort_by(|left, right| {
            right
                .children_weight
                .cmp(&left.children_weight)
                .then_with(|| right.self_weight.cmp(&left.self_weight))
        }),
        Sort::Symbol => rows.sort_by(|left, right| {
            profile
                .frames
                .symbol_of(left.frame)
                .cmp(profile.frames.symbol_of(right.frame))
        }),
        Sort::Object => rows.sort_by(|left, right| {
            let left_object = profile.frames.text(profile.frames.frame(left.frame).object);
            let right_object = profile
                .frames
                .text(profile.frames.frame(right.frame).object);
            left_object
                .cmp(right_object)
                .then_with(|| right.self_weight.cmp(&left.self_weight))
        }),
    }
}

/// A percentage as every report column prints one.
#[must_use]
pub fn percent(part: u64, total: u64) -> String {
    format!("{:>7.2}%", 100.0 * share(part, total))
}

/// The command column: the program a recording ran.
#[must_use]
pub fn command_column(command: &str) -> String {
    let trimmed = command.trim();
    if trimmed.is_empty() {
        "kira".to_owned()
    } else {
        trimmed.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Frame, Nanos, Sample, View};

    fn profile() -> Profile {
        let mut profile = Profile::new(View::Kira, "kira-wall", "test");
        let object = profile.frames.name("[vm]");
        let main = profile.frames.name("main");
        let step = profile.frames.name("step");
        let main = profile
            .frames
            .insert(Frame::named(main, FrameKind::Kira, object));
        let step = profile
            .frames
            .insert(Frame::named(step, FrameKind::Kira, object));
        profile.samples.push(Sample {
            thread: ThreadId::new(0),
            time: Nanos::new(0),
            weight: 30,
            stack: vec![main],
        });
        profile.samples.push(Sample {
            thread: ThreadId::new(0),
            time: Nanos::new(1),
            weight: 70,
            stack: vec![main, step],
        });
        profile
    }

    #[test]
    fn a_caller_carries_its_callees_weight_but_not_their_self_time() {
        let aggregate = aggregate(&profile(), &ReportOptions::default());
        assert_eq!(aggregate.total, 100);
        let main = aggregate
            .rows
            .iter()
            .find(|row| row.children_weight == 100)
            .expect("main is on every stack");
        assert_eq!(main.self_weight, 30);
        let step = aggregate
            .rows
            .iter()
            .find(|row| row.self_weight == 70)
            .expect("step is the leaf of the second sample");
        assert_eq!(step.children_weight, 70);
    }

    #[test]
    fn recursion_counts_a_sample_once_in_the_children_column() {
        let mut profile = profile();
        let frame = profile.samples[1].stack[1];
        profile.samples.push(Sample {
            thread: ThreadId::new(0),
            time: Nanos::new(2),
            weight: 100,
            stack: vec![frame, frame, frame],
        });
        let aggregate = aggregate(&profile, &ReportOptions::default());
        let step = aggregate
            .rows
            .iter()
            .find(|row| profile.frames.symbol_of(row.frame) == "step")
            .expect("step has a row");
        assert_eq!(step.children_weight, 170);
    }

    #[test]
    fn a_symbol_filter_keeps_only_the_rows_that_match() {
        let options = ReportOptions {
            symbol: Some("ste".to_owned()),
            ..ReportOptions::default()
        };
        let aggregate = aggregate(&profile(), &options);
        assert_eq!(aggregate.rows.len(), 1);
    }
}
