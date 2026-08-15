//! The recorded trace: what `record` writes and every other verb reads.
//!
//! One run produces one file holding every view of it, so `report`, `annotate`,
//! `script`, `stat`, and `diff` all work from the same recording and none of
//! them has to run the program again.
//!
//! The format is line-oriented UTF-8 text. That is a deliberate choice for a
//! tool an agent drives: a trace can be grepped, diffed, and truncated with
//! ordinary tools, and a reader that does not understand a record can say which
//! line it choked on. Every line is a record name followed by positional words
//! and `key=value` fields; values that contain a space are quoted, with `\\`,
//! `\"`, and `\n` as the only escapes.

use std::io::Write;
use std::path::{Path, PathBuf};

use kira_debug::Backend;

use crate::model::{Frame, FrameKind, Nanos, Profile, Sample, ThreadId, ThreadRecord, Unit, View};

/// The format marker every trace starts with.
const MAGIC: &str = "kira-profile";

/// The format version this build writes and reads.
const VERSION: u32 = 1;

/// What was recorded, beyond the samples themselves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraceMeta {
    /// The program's name, which reports print in the command column.
    pub command: String,
    /// The arguments the program was given.
    pub arguments: Vec<String>,
    /// The engine the program ran on.
    pub backend: Backend,
    /// The source the program was compiled from.
    pub source: Option<PathBuf>,
    /// Wall-clock start of the run, in milliseconds since the Unix epoch.
    pub started_unix_ms: u64,
    /// How long the recorded window lasted.
    pub duration: Nanos,
    /// The exit code the program reported.
    pub exit_code: i32,
}

/// One recording: what was run, and every view of how it spent its time.
#[derive(Debug)]
pub struct Trace {
    /// What was recorded.
    pub meta: TraceMeta,
    /// The views, in the order they were collected.
    pub profiles: Vec<Profile>,
}

impl Trace {
    /// The profile holding `view`'s stacks, when the recording has it.
    #[must_use]
    pub fn view(&self, view: View) -> Option<&Profile> {
        self.profiles.iter().find(|profile| profile.view == view)
    }

    /// Writes the trace to `path`.
    pub fn save(&self, path: &Path) -> Result<(), TraceError> {
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent).map_err(|source| TraceError::Write {
                path: path.to_path_buf(),
                source,
            })?;
        }
        let file = std::fs::File::create(path).map_err(|source| TraceError::Write {
            path: path.to_path_buf(),
            source,
        })?;
        let mut writer = std::io::BufWriter::new(file);
        self.write(&mut writer).map_err(|source| TraceError::Write {
            path: path.to_path_buf(),
            source,
        })
    }

    /// Reads a trace from `path`.
    pub fn load(path: &Path) -> Result<Trace, TraceError> {
        let text = std::fs::read_to_string(path).map_err(|source| TraceError::Read {
            path: path.to_path_buf(),
            source,
        })?;
        Trace::parse(&text)
    }

    /// Writes the trace as text.
    pub fn write(&self, out: &mut impl Write) -> std::io::Result<()> {
        writeln!(out, "{MAGIC} {VERSION}")?;
        writeln!(out, "meta command {}", quote(&self.meta.command))?;
        for argument in &self.meta.arguments {
            writeln!(out, "meta argument {}", quote(argument))?;
        }
        writeln!(out, "meta backend {}", self.meta.backend.label())?;
        if let Some(source) = &self.meta.source {
            writeln!(out, "meta source {}", quote(&source.to_string_lossy()))?;
        }
        writeln!(out, "meta started-unix-ms {}", self.meta.started_unix_ms)?;
        writeln!(out, "meta duration {}", self.meta.duration.get())?;
        writeln!(out, "meta exit-code {}", self.meta.exit_code)?;
        for profile in &self.profiles {
            write_profile(profile, out)?;
        }
        Ok(())
    }

    /// Parses a trace from text.
    pub fn parse(text: &str) -> Result<Trace, TraceError> {
        Reader::new(text).run()
    }
}

fn write_profile(profile: &Profile, out: &mut impl Write) -> std::io::Result<()> {
    writeln!(
        out,
        "profile {} event={} unit={} collector={} frequency={} lost={}",
        profile.view.label(),
        quote(&profile.event),
        profile.unit.label(),
        quote(&profile.collector),
        profile.frequency,
        profile.lost,
    )?;
    for thread in &profile.threads {
        writeln!(
            out,
            "thread {} name={}",
            thread.id.index(),
            quote(&thread.name)
        )?;
    }
    for (index, frame) in profile.frames.frames().iter().enumerate() {
        write!(
            out,
            "frame {index} kind={} name={} object={}",
            frame.kind.label(),
            quote(profile.frames.text(frame.symbol)),
            quote(profile.frames.text(frame.object)),
        )?;
        if let Some(function) = frame.function {
            write!(out, " function={function}")?;
        }
        if let Some(offset) = frame.offset {
            write!(out, " offset={offset}")?;
        }
        if let Some(file) = frame.file {
            write!(out, " file={}", quote(profile.frames.text(file)))?;
        }
        if let Some(line) = frame.line {
            write!(out, " line={line}")?;
        }
        writeln!(out)?;
    }
    for sample in &profile.samples {
        let stack = sample
            .stack
            .iter()
            .map(|frame| frame.index().to_string())
            .collect::<Vec<_>>()
            .join(",");
        writeln!(
            out,
            "sample thread={} time={} weight={} stack={stack}",
            sample.thread.index(),
            sample.time.get(),
            sample.weight,
        )?;
    }
    Ok(())
}

/// Why a trace could not be read or written.
#[derive(Debug, thiserror::Error)]
pub enum TraceError {
    /// The file could not be read.
    #[error("cannot read the profile `{path}`: {source}")]
    Read {
        /// The trace file.
        path: PathBuf,
        /// The underlying I/O failure.
        #[source]
        source: std::io::Error,
    },
    /// The file could not be written.
    #[error("cannot write the profile `{path}`: {source}")]
    Write {
        /// The trace file.
        path: PathBuf,
        /// The underlying I/O failure.
        #[source]
        source: std::io::Error,
    },
    /// The file does not start with the format marker.
    #[error("this is not a Kira profile: expected a first line of `{MAGIC} <version>`")]
    NotATrace,
    /// The file was written by a different format version.
    #[error("this profile is version {found}; this build reads version {VERSION}")]
    Version {
        /// The version the file declares.
        found: u32,
    },
    /// A record could not be understood.
    #[error("profile line {line}: {problem}")]
    Malformed {
        /// The one-based line number.
        line: usize,
        /// What was wrong with it.
        problem: String,
    },
}

/// Renders a value as one word, quoting it when it is not already one.
fn quote(text: &str) -> String {
    let plain = !text.is_empty()
        && text
            .chars()
            .all(|character| !character.is_whitespace() && character != '"' && character != '\\');
    if plain {
        return text.to_owned();
    }
    let mut quoted = String::with_capacity(text.len() + 2);
    quoted.push('"');
    for character in text.chars() {
        match character {
            '"' => quoted.push_str("\\\""),
            '\\' => quoted.push_str("\\\\"),
            '\n' => quoted.push_str("\\n"),
            other => quoted.push(other),
        }
    }
    quoted.push('"');
    quoted
}

/// Splits a record into its words, keeping quoted runs whole.
fn words(line: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    let mut escaped = false;
    let mut started = false;
    for character in line.chars() {
        if escaped {
            current.push(match character {
                'n' => '\n',
                other => other,
            });
            escaped = false;
            continue;
        }
        match character {
            '\\' if quoted => escaped = true,
            '"' => {
                quoted = !quoted;
                started = true;
            }
            character if character.is_whitespace() && !quoted => {
                if started || !current.is_empty() {
                    words.push(std::mem::take(&mut current));
                    started = false;
                }
            }
            other => {
                current.push(other);
                started = true;
            }
        }
    }
    if started || !current.is_empty() {
        words.push(current);
    }
    words
}

/// A `key=value` field, split once on the first `=`.
fn field(word: &str) -> Option<(&str, &str)> {
    word.split_once('=')
}

/// The reader's state while it walks a trace's records.
struct Reader<'a> {
    text: &'a str,
    meta: TraceMeta,
    profiles: Vec<Profile>,
}

impl<'a> Reader<'a> {
    fn new(text: &'a str) -> Self {
        Self {
            text,
            meta: TraceMeta {
                command: String::new(),
                arguments: Vec::new(),
                backend: Backend::Vm,
                source: None,
                started_unix_ms: 0,
                duration: Nanos::ZERO,
                exit_code: 0,
            },
            profiles: Vec::new(),
        }
    }

    fn run(mut self) -> Result<Trace, TraceError> {
        let mut lines = self.text.lines().enumerate();
        let Some((_, header)) = lines.next() else {
            return Err(TraceError::NotATrace);
        };
        let header = words(header);
        match (header.first().map(String::as_str), header.get(1)) {
            (Some(MAGIC), Some(version)) => {
                let found = version
                    .parse::<u32>()
                    .map_err(|_| TraceError::Version { found: 0 })?;
                if found != VERSION {
                    return Err(TraceError::Version { found });
                }
            }
            _ => return Err(TraceError::NotATrace),
        }

        for (index, line) in lines {
            let number = index + 1;
            let words = words(line);
            let Some(record) = words.first().map(String::as_str) else {
                continue;
            };
            match record {
                "meta" => self.meta_record(number, &words)?,
                "profile" => self.profile_record(number, &words)?,
                "thread" => self.thread_record(number, &words)?,
                "frame" => self.frame_record(number, &words)?,
                "sample" => self.sample_record(number, &words)?,
                other => {
                    return Err(TraceError::Malformed {
                        line: number,
                        problem: format!("unknown record `{other}`"),
                    });
                }
            }
        }
        Ok(Trace {
            meta: self.meta,
            profiles: self.profiles,
        })
    }

    fn meta_record(&mut self, line: usize, words: &[String]) -> Result<(), TraceError> {
        let key = words.get(1).map(String::as_str).unwrap_or_default();
        let value = words.get(2).cloned().unwrap_or_default();
        match key {
            "command" => self.meta.command = value,
            "argument" => self.meta.arguments.push(value),
            "backend" => {
                self.meta.backend = parse_backend(&value).ok_or_else(|| TraceError::Malformed {
                    line,
                    problem: format!("unknown backend `{value}`"),
                })?;
            }
            "source" => self.meta.source = Some(PathBuf::from(value)),
            "started-unix-ms" => self.meta.started_unix_ms = number(line, &value)?,
            "duration" => self.meta.duration = Nanos::new(number(line, &value)?),
            "exit-code" => {
                self.meta.exit_code = value.parse().map_err(|_| TraceError::Malformed {
                    line,
                    problem: format!("`{value}` is not an exit code"),
                })?;
            }
            other => {
                return Err(TraceError::Malformed {
                    line,
                    problem: format!("unknown meta key `{other}`"),
                });
            }
        }
        Ok(())
    }

    fn profile_record(&mut self, line: usize, words: &[String]) -> Result<(), TraceError> {
        let label = words.get(1).map(String::as_str).unwrap_or_default();
        let view = View::parse(label).ok_or_else(|| TraceError::Malformed {
            line,
            problem: format!("unknown view `{label}`"),
        })?;
        let mut profile = Profile::new(view, String::new(), String::new());
        for word in &words[2.min(words.len())..] {
            match field(word) {
                Some(("event", value)) => profile.event = value.to_owned(),
                Some(("unit", value)) => {
                    profile.unit = Unit::parse(value).ok_or_else(|| TraceError::Malformed {
                        line,
                        problem: format!("unknown weight unit `{value}`"),
                    })?;
                }
                Some(("collector", value)) => profile.collector = value.to_owned(),
                Some(("frequency", value)) => profile.frequency = number(line, value)? as u32,
                Some(("lost", value)) => profile.lost = number(line, value)?,
                _ => {
                    return Err(TraceError::Malformed {
                        line,
                        problem: format!("unknown profile field `{word}`"),
                    });
                }
            }
        }
        self.profiles.push(profile);
        Ok(())
    }

    fn current(&mut self, line: usize) -> Result<&mut Profile, TraceError> {
        self.profiles.last_mut().ok_or(TraceError::Malformed {
            line,
            problem: "this record comes before any `profile` line".to_owned(),
        })
    }

    fn thread_record(&mut self, line: usize, words: &[String]) -> Result<(), TraceError> {
        let id =
            ThreadId::new(number(line, words.get(1).map(String::as_str).unwrap_or(""))? as u32);
        let mut name = String::new();
        for word in &words[2.min(words.len())..] {
            if let Some(("name", value)) = field(word) {
                name = value.to_owned();
            }
        }
        self.current(line)?.threads.push(ThreadRecord { id, name });
        Ok(())
    }

    fn frame_record(&mut self, line: usize, words: &[String]) -> Result<(), TraceError> {
        let profile = self.profiles.last_mut().ok_or(TraceError::Malformed {
            line,
            problem: "this record comes before any `profile` line".to_owned(),
        })?;
        let mut kind = FrameKind::Unknown;
        let mut symbol = None;
        let mut object = None;
        let mut function = None;
        let mut offset = None;
        let mut file = None;
        let mut source_line = None;
        for word in &words[2.min(words.len())..] {
            match field(word) {
                Some(("kind", value)) => {
                    kind = FrameKind::parse(value).ok_or_else(|| TraceError::Malformed {
                        line,
                        problem: format!("unknown frame kind `{value}`"),
                    })?;
                }
                Some(("name", value)) => symbol = Some(profile.frames.name(value)),
                Some(("object", value)) => object = Some(profile.frames.name(value)),
                Some(("function", value)) => function = Some(number(line, value)? as u32),
                Some(("offset", value)) => offset = Some(number(line, value)? as u32),
                Some(("file", value)) => file = Some(profile.frames.name(value)),
                Some(("line", value)) => source_line = Some(number(line, value)? as u32),
                _ => {
                    return Err(TraceError::Malformed {
                        line,
                        problem: format!("unknown frame field `{word}`"),
                    });
                }
            }
        }
        let empty = profile.frames.name("");
        profile.frames.insert(Frame {
            symbol: symbol.unwrap_or(empty),
            kind,
            object: object.unwrap_or(empty),
            function,
            offset,
            file,
            line: source_line,
        });
        Ok(())
    }

    fn sample_record(&mut self, line: usize, words: &[String]) -> Result<(), TraceError> {
        let profile = self.profiles.last_mut().ok_or(TraceError::Malformed {
            line,
            problem: "this record comes before any `profile` line".to_owned(),
        })?;
        let mut sample = Sample {
            thread: ThreadId::new(0),
            time: Nanos::ZERO,
            weight: 0,
            stack: Vec::new(),
        };
        for word in &words[1.min(words.len())..] {
            match field(word) {
                Some(("thread", value)) => {
                    sample.thread = ThreadId::new(number(line, value)? as u32)
                }
                Some(("time", value)) => sample.time = Nanos::new(number(line, value)?),
                Some(("weight", value)) => sample.weight = number(line, value)?,
                Some(("stack", value)) => {
                    for entry in value.split(',').filter(|entry| !entry.is_empty()) {
                        let raw = number(line, entry)? as u32;
                        let id = profile
                            .frames
                            .id(raw)
                            .ok_or_else(|| TraceError::Malformed {
                                line,
                                problem: format!("frame {raw} is not in this profile"),
                            })?;
                        sample.stack.push(id);
                    }
                }
                _ => {
                    return Err(TraceError::Malformed {
                        line,
                        problem: format!("unknown sample field `{word}`"),
                    });
                }
            }
        }
        profile.samples.push(sample);
        Ok(())
    }
}

/// Parses an unsigned field, naming the line when it is not a number.
fn number(line: usize, value: &str) -> Result<u64, TraceError> {
    value.parse::<u64>().map_err(|_| TraceError::Malformed {
        line,
        problem: format!("`{value}` is not a number"),
    })
}

/// The backend a trace's word names.
///
/// The spelling is [`Backend::label`]'s, so a trace and a `--backend` flag
/// always agree.
fn parse_backend(label: &str) -> Option<Backend> {
    [Backend::Vm, Backend::Hybrid, Backend::Llvm]
        .into_iter()
        .find(|backend| backend.label() == label)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_trace() -> Trace {
        let mut profile = Profile::new(View::Kira, "kira-wall", "kira-runtime");
        profile.frequency = 1000;
        profile.threads.push(ThreadRecord {
            id: ThreadId::new(0),
            name: "main thread".to_owned(),
        });
        let name = profile.frames.name("Grid.step");
        let object = profile.frames.name("[vm]");
        let file = profile.frames.name("src/main.kira");
        let outer = profile.frames.insert(Frame {
            symbol: name,
            kind: FrameKind::Kira,
            object,
            function: Some(3),
            offset: Some(17),
            file: Some(file),
            line: Some(12),
        });
        let leaf_name = profile.frames.name("Cell.\"weird\" name");
        let leaf = profile
            .frames
            .insert(Frame::named(leaf_name, FrameKind::Runtime, object));
        profile.samples.push(Sample {
            thread: ThreadId::new(0),
            time: Nanos::new(1_000_000),
            weight: 1_000_000,
            stack: vec![outer, leaf],
        });
        Trace {
            meta: TraceMeta {
                command: "hello".to_owned(),
                arguments: vec!["--rows".to_owned(), "3".to_owned()],
                backend: Backend::Vm,
                source: Some(PathBuf::from("src/main.kira")),
                started_unix_ms: 1_755_000_000_000,
                duration: Nanos::new(2_000_000_000),
                exit_code: 0,
            },
            profiles: vec![profile],
        }
    }

    #[test]
    fn a_written_trace_reads_back_identical() {
        let trace = sample_trace();
        let mut text = Vec::new();
        trace.write(&mut text).expect("write the trace");
        let text = String::from_utf8(text).expect("the trace is text");
        let parsed = Trace::parse(&text).expect("read the trace back");

        assert_eq!(parsed.meta, trace.meta);
        let original = trace.view(View::Kira).expect("the kira view");
        let round_tripped = parsed.view(View::Kira).expect("the kira view");
        assert_eq!(round_tripped.event, original.event);
        assert_eq!(round_tripped.frequency, original.frequency);
        assert_eq!(round_tripped.threads, original.threads);
        assert_eq!(round_tripped.samples, original.samples);
        assert_eq!(
            round_tripped
                .frames
                .symbol_of(round_tripped.samples[0].stack[1]),
            "Cell.\"weird\" name"
        );
    }

    #[test]
    fn a_file_that_is_not_a_trace_is_refused_by_its_first_line() {
        assert!(matches!(
            Trace::parse("hello\n"),
            Err(TraceError::NotATrace)
        ));
        assert!(matches!(
            Trace::parse("kira-profile 99\n"),
            Err(TraceError::Version { found: 99 })
        ));
    }

    #[test]
    fn a_sample_naming_a_frame_the_profile_does_not_have_is_refused_by_line() {
        let text = "kira-profile 1\nprofile kira\nsample thread=0 time=0 weight=1 stack=4\n";
        match Trace::parse(text) {
            Err(TraceError::Malformed { line, problem }) => {
                assert_eq!(line, 3);
                assert!(problem.contains("frame 4"), "{problem}");
            }
            other => panic!("expected a malformed-record error, got {other:?}"),
        }
    }

    #[test]
    fn words_keep_quoted_runs_whole() {
        assert_eq!(
            words(r#"frame 0 name="a b" object=c"#),
            vec!["frame", "0", "name=a b", "object=c"]
        );
    }
}
