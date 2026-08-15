//! Sampling with `perf`, the Linux profiler.
//!
//! `perf record` launches the program and samples it; `perf script` reads the
//! recording back as text. Kira drives both rather than opening
//! `perf_event_open` itself, because `perf` already knows how to unwind every
//! stack on the machine, resolve every symbol format, and follow the program's
//! children — and because a developer who knows `perf` can point their own
//! `perf` at the same recording.
//!
//! The recording is placed in the system temporary directory and removed when
//! the profile has been read, so a run leaves nothing behind.

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};

use crate::collect::{CollectError, CollectOptions, Launch};
use crate::model::{Frame, Nanos, Profile, Sample, ThreadId, ThreadRecord, View};
use crate::symbols::KiraSymbols;

/// A `perf record` that is running the program.
#[derive(Debug)]
pub(super) struct Recorder {
    perf: Child,
    data: PathBuf,
    frequency: u32,
}

impl Recorder {
    /// The name a report gives this collector.
    pub(super) const TOOL: &'static str = "perf";

    pub(super) fn start(launch: &Launch, options: &CollectOptions) -> Result<Self, CollectError> {
        if which("perf").is_none() {
            return Err(CollectError::Unavailable {
                tool: Self::TOOL,
                reason: "`perf` is not on PATH; install the distribution's linux-tools package"
                    .to_owned(),
            });
        }
        let data = std::env::temp_dir().join(format!("kira-perf-{}.data", std::process::id()));
        let mut command = Command::new("perf");
        command
            .arg("record")
            .arg("--quiet")
            .arg("--freq")
            .arg(options.frequency.to_string())
            .arg("--event")
            .arg("cpu-clock")
            .arg("--output")
            .arg(&data);
        if options.call_graph {
            // Frame pointers first, DWARF where there are none: the same pair
            // `perf` itself recommends, and the reason a Kira native build
            // keeps its unwind tables.
            command.arg("--call-graph").arg("fp");
        }
        command.arg("--");
        command.arg(&launch.program);
        command.args(&launch.arguments);
        for (key, value) in &launch.environment {
            command.env(key, value);
        }
        let perf = command.spawn().map_err(|source| CollectError::Spawn {
            program: PathBuf::from("perf"),
            source,
        })?;
        Ok(Self {
            perf,
            data,
            frequency: options.frequency,
        })
    }

    pub(super) fn wait(&mut self) -> Result<i32, CollectError> {
        // `perf record` forwards the program's own exit status, so waiting for
        // `perf` is waiting for the program.
        let status = self.perf.wait().map_err(|source| CollectError::Io {
            action: "waiting for perf".to_owned(),
            source,
        })?;
        Ok(status.code().unwrap_or(1))
    }

    pub(super) fn finish(self, symbols: &KiraSymbols) -> Result<Profile, CollectError> {
        let output = Command::new("perf")
            .arg("script")
            .arg("--input")
            .arg(&self.data)
            .arg("--fields")
            .arg("comm,tid,time,period,ip,sym,dso")
            .stderr(Stdio::piped())
            .output()
            .map_err(|source| CollectError::Spawn {
                program: PathBuf::from("perf"),
                source,
            })?;
        let _ = std::fs::remove_file(&self.data);
        if !output.status.success() {
            return Err(CollectError::Tool {
                tool: Self::TOOL,
                problem: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
            });
        }
        let text = String::from_utf8_lossy(&output.stdout).into_owned();
        Ok(parse(&text, self.frequency, symbols))
    }
}

/// The path of `program` on `PATH`, if it is there.
fn which(program: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|directory| directory.join(program))
        .find(|candidate| candidate.is_file())
}

/// Reads `perf script` output into a profile.
///
/// The format is one blank-line-separated record per sample: a header naming
/// the command, thread, time, period, and event, then one indented frame per
/// line from the innermost outwards.
fn parse(text: &str, frequency: u32, symbols: &KiraSymbols) -> Profile {
    let mut profile = Profile::new(View::Machine, "cpu-clock", Recorder::TOOL);
    profile.frequency = frequency;
    let mut threads: Vec<String> = Vec::new();
    let mut current: Option<Pending> = None;

    for line in text.lines() {
        if line.trim().is_empty() {
            finish_sample(&mut profile, &mut current);
            continue;
        }
        if line.starts_with(char::is_whitespace) {
            if let Some(pending) = current.as_mut() {
                pending.frames.push(parse_frame(line));
            }
            continue;
        }
        finish_sample(&mut profile, &mut current);
        current = parse_header(line, &mut threads);
    }
    finish_sample(&mut profile, &mut current);

    for (index, name) in threads.iter().enumerate() {
        profile.threads.push(ThreadRecord {
            id: ThreadId::new(index as u32),
            name: name.clone(),
        });
    }
    resymbolize(&mut profile, symbols);
    profile
}

/// A sample being read, before its frames arrive.
#[derive(Debug)]
struct Pending {
    thread: u32,
    time: Nanos,
    weight: u64,
    frames: Vec<(String, String)>,
}

/// Reads `comm tid time: period event:` into a pending sample.
fn parse_header(line: &str, threads: &mut Vec<String>) -> Option<Pending> {
    let mut words = line.split_whitespace();
    let comm = words.next()?;
    let tid = words.next()?;
    let time = words.next()?.trim_end_matches(':');
    let period = words.next().unwrap_or("0");
    let seconds = time.parse::<f64>().ok()?;
    let name = format!("{comm}-{tid}");
    let thread = match threads.iter().position(|known| *known == name) {
        Some(index) => index as u32,
        None => {
            threads.push(name);
            (threads.len() - 1) as u32
        }
    };
    Some(Pending {
        thread,
        time: Nanos::new((seconds * 1e9) as u64),
        weight: period.parse::<u64>().unwrap_or(0),
        frames: Vec::new(),
    })
}

/// Reads `    <address> <symbol>+<offset> (<dso>)` into a symbol and an image.
fn parse_frame(line: &str) -> (String, String) {
    let trimmed = line.trim();
    let (body, object) = match trimmed.rsplit_once(" (") {
        Some((body, object)) => (body, object.trim_end_matches(')')),
        None => (trimmed, "[unknown]"),
    };
    let symbol = body
        .split_once(' ')
        .map(|(_address, rest)| rest)
        .unwrap_or(body);
    let symbol = symbol.split_once('+').map_or(symbol, |(name, _)| name);
    (symbol.trim().to_owned(), object.trim().to_owned())
}

fn finish_sample(profile: &mut Profile, pending: &mut Option<Pending>) {
    let Some(pending) = pending.take() else {
        return;
    };
    if pending.frames.is_empty() {
        return;
    }
    let mut stack = Vec::with_capacity(pending.frames.len());
    // `perf script` prints innermost first; a profile stores outermost first.
    for (symbol, object) in pending.frames.iter().rev() {
        let name = profile.frames.name(symbol);
        let image = profile.frames.name(object);
        stack.push(profile.frames.insert(Frame::named(
            name,
            crate::model::FrameKind::Unknown,
            image,
        )));
    }
    profile.samples.push(Sample {
        thread: ThreadId::new(pending.thread),
        time: pending.time,
        weight: pending.weight,
        stack,
    });
}

/// Gives every frame its kind and its Kira identity.
///
/// Done in one pass at the end rather than per frame while parsing, because a
/// frame table already holds each distinct frame exactly once.
fn resymbolize(profile: &mut Profile, symbols: &KiraSymbols) {
    let classified = profile
        .frames
        .frames()
        .iter()
        .map(|frame| {
            let symbol = profile.frames.text(frame.symbol).to_owned();
            let object = profile.frames.text(frame.object).to_owned();
            symbols.classify(&symbol, &object)
        })
        .collect::<Vec<_>>();
    let mut replacement = crate::model::FrameTable::new();
    let mut mapping = Vec::with_capacity(classified.len());
    for (frame, identity) in profile.frames.frames().iter().zip(&classified) {
        let name = replacement.name(&identity.name);
        let object = replacement.name(profile.frames.text(frame.object));
        mapping.push(replacement.insert(Frame {
            symbol: name,
            kind: identity.kind,
            object,
            function: identity.function,
            ..*frame
        }));
    }
    for sample in &mut profile.samples {
        for frame in &mut sample.stack {
            if let Some(mapped) = mapping.get(frame.index() as usize) {
                *frame = *mapped;
            }
        }
    }
    profile.frames = replacement;
}

#[cfg(test)]
mod tests {
    use super::*;
    use kira_debug::{Backend, DebugFunction, DebugInfo};

    fn symbols() -> KiraSymbols {
        KiraSymbols::from_debug(&DebugInfo {
            module_name: "hello".to_owned(),
            backend: Backend::Llvm,
            source: None,
            functions: vec![DebugFunction {
                id: 4,
                name: "Grid.step".to_owned(),
                backend: Backend::Llvm,
                symbol: Some("kira_fn_4_Grid_step".to_owned()),
                line: 12,
            }],
            optimized: false,
        })
    }

    #[test]
    fn a_script_record_becomes_one_sample_with_its_stack_outermost_first() {
        let text = "\
hello 1234 1.002000: 1000000 cpu-clock:
\t    55e0ac kira_fn_4_Grid_step+0x1c (/tmp/hello)
\t    55e100 kira_fn_0_main+0x40 (/tmp/hello)
\t    7f0011 __libc_start_main+0x80 (/lib/x86_64-linux-gnu/libc.so.6)

";
        let profile = parse(text, 997, &symbols());
        assert_eq!(profile.samples.len(), 1);
        let sample = &profile.samples[0];
        assert_eq!(sample.weight, 1_000_000);
        assert_eq!(sample.time, Nanos::new(1_002_000_000));
        assert_eq!(profile.frames.symbol_of(sample.stack[2]), "Grid.step");
        assert_eq!(
            profile.frames.frame(sample.stack[2]).kind,
            crate::model::FrameKind::Kira
        );
        assert_eq!(
            profile.frames.frame(sample.stack[0]).kind,
            crate::model::FrameKind::System
        );
        assert_eq!(profile.thread_name(sample.thread), "hello-1234");
    }

    #[test]
    fn a_frame_line_keeps_the_symbol_and_the_image_and_drops_the_offset() {
        assert_eq!(
            parse_frame("\t 55e0ac kira_rt_string_new+0x1c (/tmp/hello)"),
            ("kira_rt_string_new".to_owned(), "/tmp/hello".to_owned())
        );
        assert_eq!(
            parse_frame("\t 55e0ac [unknown]"),
            ("[unknown]".to_owned(), "[unknown]".to_owned())
        );
    }
}
