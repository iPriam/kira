//! `kira profile annotate`: where inside one function the time went.
//!
//! `perf annotate` breaks a symbol down by machine instruction. This breaks it
//! down by whatever the backend's instruction is: a bytecode instruction index
//! for an interpreted function, a byte offset into the machine code for a
//! native one — and the source line beside it whenever the recording knew one.
//!
//! The disassembly itself belongs to whoever holds the program, so a caller
//! supplies it through [`SiteText`]. A recording alone still annotates: it just
//! prints offsets without the instruction beside them.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use crate::model::{FrameId, Profile, share};
use crate::render::{ReportOptions, percent};

/// What the instruction at one site says.
pub trait SiteText {
    /// The instruction at `offset` of Kira function `function`, when known.
    fn text(&self, function: u32, offset: u32) -> Option<String>;
}

/// A recording with no program beside it: offsets, and nothing to read.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoSiteText;

impl SiteText for NoSiteText {
    fn text(&self, _function: u32, _offset: u32) -> Option<String> {
        None
    }
}

/// One annotated location inside a function.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Site {
    offset: Option<u32>,
    weight: u64,
    samples: u64,
    frame: FrameId,
}

/// Renders one symbol's instruction-level breakdown.
///
/// `symbol` is matched the way `perf annotate --symbol` matches: exactly first,
/// and by containment when nothing matched exactly, so a Kira method can be
/// asked for by its short name.
#[must_use]
pub fn render(
    profile: &Profile,
    symbol: &str,
    options: &ReportOptions,
    text: &dyn SiteText,
) -> String {
    let Some(name) = resolve_symbol(profile, symbol) else {
        return format!("# no symbol matching `{symbol}` in this recording\n");
    };
    let mut sites: BTreeMap<Option<u32>, Site> = BTreeMap::new();
    let mut total = 0u64;
    let mut whole = 0u64;
    for sample in &profile.samples {
        if options.thread.is_some_and(|thread| thread != sample.thread) {
            continue;
        }
        whole = whole.saturating_add(sample.weight);
        let Some(leaf) = sample.leaf() else {
            continue;
        };
        if profile.frames.symbol_of(leaf) != name {
            continue;
        }
        total = total.saturating_add(sample.weight);
        let entry = profile.frames.frame(leaf);
        let site = sites.entry(entry.offset).or_insert(Site {
            offset: entry.offset,
            weight: 0,
            samples: 0,
            frame: leaf,
        });
        site.weight = site.weight.saturating_add(sample.weight);
        site.samples += 1;
    }

    let mut out = String::new();
    let _ = writeln!(
        out,
        "# Annotation of {name}: {} of the recording, {} samples",
        percent(total, whole).trim(),
        sites.values().map(|site| site.samples).sum::<u64>(),
    );
    if sites.is_empty() {
        let _ = writeln!(
            out,
            "# no sample ended in {name}; it has callees but no time of its own"
        );
        return out;
    }
    out.push_str("#\n");
    out.push_str("#  Percent  Samples  Offset  Source                Instruction\n");
    out.push_str("#  .......  .......  ......  ....................  ...........\n");
    out.push_str("#\n");
    for site in sites.values() {
        if 100.0 * share(site.weight, total) < options.percent_limit {
            continue;
        }
        let frame = profile.frames.frame(site.frame);
        let source = match (frame.file, frame.line) {
            (Some(file), Some(line)) => {
                let path = profile.frames.text(file);
                let name = std::path::Path::new(path)
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_else(|| path.to_owned());
                format!("{name}:{line}")
            }
            _ => String::new(),
        };
        let instruction = frame
            .function
            .zip(site.offset)
            .and_then(|(function, offset)| text.text(function, offset))
            .unwrap_or_default();
        let offset = site
            .offset
            .map(|offset| offset.to_string())
            .unwrap_or_else(|| "-".to_owned());
        let _ = writeln!(
            out,
            "  {}  {:>7}  {offset:>6}  {source:<20}  {instruction}",
            percent(site.weight, total),
            site.samples,
        );
    }
    out
}

/// The recorded symbol `wanted` names.
fn resolve_symbol<'a>(profile: &'a Profile, wanted: &str) -> Option<&'a str> {
    let mut fallback = None;
    for frame in profile.frames.frames() {
        let name = profile.frames.text(frame.symbol);
        if name == wanted {
            return Some(name);
        }
        if fallback.is_none() && name.contains(wanted) {
            fallback = Some(name);
        }
    }
    fallback
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Frame, FrameKind, Nanos, Sample, ThreadId, View};

    struct Bytecode;

    impl SiteText for Bytecode {
        fn text(&self, function: u32, offset: u32) -> Option<String> {
            (function == 1).then(|| format!("LoadLocal {offset}"))
        }
    }

    fn profile() -> Profile {
        let mut profile = Profile::new(View::Kira, "kira-wall", "test");
        let object = profile.frames.name("[vm]");
        let name = profile.frames.name("Grid.step");
        let file = profile.frames.name("src/main.kira");
        let hot = profile.frames.insert(Frame {
            function: Some(1),
            offset: Some(7),
            file: Some(file),
            line: Some(24),
            ..Frame::named(name, FrameKind::Kira, object)
        });
        let cold = profile.frames.insert(Frame {
            function: Some(1),
            offset: Some(9),
            file: Some(file),
            line: Some(25),
            ..Frame::named(name, FrameKind::Kira, object)
        });
        for (frame, weight) in [(hot, 80u64), (cold, 20)] {
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
    fn each_instruction_gets_its_share_of_the_functions_time() {
        let text = render(
            &profile(),
            "Grid.step",
            &ReportOptions::default(),
            &Bytecode,
        );
        assert!(text.contains("Annotation of Grid.step: 100.00%"), "{text}");
        assert!(text.contains("main.kira:24"), "{text}");
        assert!(text.contains("LoadLocal 7"), "{text}");
        let hot = text.lines().find(|line| line.contains("LoadLocal 7"));
        assert!(hot.is_some_and(|line| line.contains("80.00%")), "{text}");
    }

    #[test]
    fn a_symbol_can_be_named_by_part_of_itself() {
        let text = render(&profile(), "step", &ReportOptions::default(), &NoSiteText);
        assert!(text.contains("Annotation of Grid.step"), "{text}");
    }

    #[test]
    fn a_symbol_the_recording_never_saw_is_said_plainly() {
        let text = render(
            &profile(),
            "nowhere",
            &ReportOptions::default(),
            &NoSiteText,
        );
        assert!(text.contains("no symbol matching `nowhere`"), "{text}");
    }
}
