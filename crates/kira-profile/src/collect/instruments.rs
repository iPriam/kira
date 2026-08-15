//! Sampling with `sample`, the macOS profiler.
//!
//! `/usr/bin/sample` is Apple's sampling profiler: it is on every Mac, it needs
//! no Xcode and no elevated session to sample a process the same user started,
//! and it symbolizes against the target's own Mach-O tables. It reports a call
//! tree of sample counts rather than a stream of timestamped samples, so this
//! collector expands the tree back into samples — one per count, along the path
//! that count belongs to, spaced at the sampling interval. The percentages that
//! come out are the ones `sample` measured; only their presentation changes.

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};

use crate::collect::{CollectError, CollectOptions, Launch};
use crate::model::{Frame, FrameKind, Nanos, Profile, Sample, ThreadId, ThreadRecord, View};
use crate::symbols::KiraSymbols;

/// The longest a recording runs before `sample` stops on its own, in seconds.
const MAX_DURATION: u32 = 3600;

/// A child being sampled.
#[derive(Debug)]
pub(super) struct Recorder {
    child: Child,
    sampler: Option<Child>,
    report: PathBuf,
    frequency: u32,
}

impl Recorder {
    /// The name a report gives this collector.
    pub(super) const TOOL: &'static str = "sample";

    pub(super) fn start(launch: &Launch, options: &CollectOptions) -> Result<Self, CollectError> {
        let mut command = Command::new(&launch.program);
        command.args(&launch.arguments);
        for (key, value) in &launch.environment {
            command.env(key, value);
        }
        let child = command.spawn().map_err(|source| CollectError::Spawn {
            program: launch.program.clone(),
            source,
        })?;

        let report = std::env::temp_dir().join(format!("kira-sample-{}.txt", std::process::id()));
        let interval = (1_000 / options.frequency.max(1)).max(1);
        let sampler = Command::new("/usr/bin/sample")
            .arg(child.id().to_string())
            .arg(MAX_DURATION.to_string())
            .arg(interval.to_string())
            .arg("-mayDie")
            .arg("-f")
            .arg(&report)
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn();
        let sampler = match sampler {
            Ok(sampler) => Some(sampler),
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
                return Err(CollectError::Unavailable {
                    tool: Self::TOOL,
                    reason: "/usr/bin/sample is not installed".to_owned(),
                });
            }
            Err(source) => {
                return Err(CollectError::Spawn {
                    program: PathBuf::from("/usr/bin/sample"),
                    source,
                });
            }
        };
        Ok(Self {
            child,
            sampler,
            report,
            frequency: options.frequency,
        })
    }

    pub(super) fn wait(&mut self) -> Result<i32, CollectError> {
        let status = self.child.wait().map_err(|source| CollectError::Io {
            action: "waiting for the program".to_owned(),
            source,
        })?;
        Ok(status.code().unwrap_or(1))
    }

    pub(super) fn finish(mut self, symbols: &KiraSymbols) -> Result<Profile, CollectError> {
        if let Some(sampler) = self.sampler.as_mut() {
            sampler.wait().map_err(|source| CollectError::Io {
                action: "waiting for sample".to_owned(),
                source,
            })?;
        }
        let text = std::fs::read_to_string(&self.report).unwrap_or_default();
        let _ = std::fs::remove_file(&self.report);
        Ok(parse(&text, self.frequency, symbols))
    }
}

/// One line of the call tree, with the depth its indentation gave it.
#[derive(Debug)]
struct Node {
    depth: usize,
    count: u64,
    symbol: String,
    object: String,
    offset: Option<u32>,
    thread: bool,
}

/// Reads a `sample` report into a profile.
fn parse(text: &str, frequency: u32, symbols: &KiraSymbols) -> Profile {
    let mut profile = Profile::new(View::Machine, "wall-clock", Recorder::TOOL);
    profile.frequency = frequency;
    let period = 1_000_000_000 / u64::from(frequency.max(1));

    let nodes = read_tree(text);
    let mut threads: Vec<String> = Vec::new();
    // The path of frames from the outermost node down to the node being read,
    // and the sample count each of them still owes to its own body.
    let mut path: Vec<(usize, crate::model::FrameId)> = Vec::new();
    let mut thread = ThreadId::new(0);
    let mut clock = 0u64;

    for (index, node) in nodes.iter().enumerate() {
        if node.thread {
            path.clear();
            let name = node.symbol.clone();
            thread = ThreadId::new(match threads.iter().position(|known| *known == name) {
                Some(known) => known as u32,
                None => {
                    threads.push(name);
                    (threads.len() - 1) as u32
                }
            });
            continue;
        }
        while path.last().is_some_and(|(depth, _)| *depth >= node.depth) {
            path.pop();
        }
        let identity = symbols.classify(&node.symbol, &node.object);
        let name = profile.frames.name(&identity.name);
        let object = profile.frames.name(&node.object);
        let frame = profile.frames.insert(Frame {
            symbol: name,
            kind: identity.kind,
            object,
            function: identity.function,
            offset: node.offset,
            file: None,
            line: None,
        });
        path.push((node.depth, frame));

        // A node's count includes its callees'; what is left is the time the
        // function spent in its own body, and that is what becomes samples.
        let own = node.count.saturating_sub(callee_count(&nodes, index));
        let stack = path.iter().map(|(_, frame)| *frame).collect::<Vec<_>>();
        for _ in 0..own {
            clock = clock.saturating_add(period);
            profile.samples.push(Sample {
                thread,
                time: Nanos::new(clock),
                weight: period,
                stack: stack.clone(),
            });
        }
    }

    for (index, name) in threads.iter().enumerate() {
        profile.threads.push(ThreadRecord {
            id: ThreadId::new(index as u32),
            name: name.clone(),
        });
    }
    profile
}

/// The counts belonging to the direct callees of the node at `index`.
///
/// A report's indentation step is the report's own, so the callees are the
/// nodes at whatever the first deeper depth in this subtree turns out to be
/// rather than at a fixed offset from the caller's.
fn callee_count(nodes: &[Node], index: usize) -> u64 {
    let Some(node) = nodes.get(index) else {
        return 0;
    };
    let mut callees = None;
    let mut total = 0u64;
    for next in nodes.iter().skip(index + 1) {
        if next.thread || next.depth <= node.depth {
            break;
        }
        let depth = *callees.get_or_insert(next.depth);
        if next.depth == depth {
            total = total.saturating_add(next.count);
        }
    }
    total
}

/// Reads the indented call tree out of a `sample` report.
fn read_tree(text: &str) -> Vec<Node> {
    let mut nodes = Vec::new();
    let mut inside = false;
    for line in text.lines() {
        if line.starts_with("Call graph:") {
            inside = true;
            continue;
        }
        if !inside {
            continue;
        }
        if line.starts_with("Total number in stack")
            || line.starts_with("Binary Images:")
            || line.starts_with("Sort by top of stack")
        {
            break;
        }
        let depth = line.len() - line.trim_start().len();
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Some((count, rest)) = trimmed.split_once(' ') else {
            continue;
        };
        let Ok(count) = count.parse::<u64>() else {
            continue;
        };
        let rest = rest.trim();
        if rest.starts_with("Thread_") {
            nodes.push(Node {
                depth,
                count,
                symbol: rest.split_whitespace().next().unwrap_or(rest).to_owned(),
                object: "[thread]".to_owned(),
                offset: None,
                thread: true,
            });
            continue;
        }
        nodes.push(read_frame(depth, count, rest));
    }
    nodes
}

/// Reads `symbol  (in image) + offset  [address]`.
fn read_frame(depth: usize, count: u64, text: &str) -> Node {
    let body = text.split_once("  [").map_or(text, |(body, _)| body);
    let (before, object) = match body.split_once("  (in ") {
        Some((before, rest)) => (before, rest.split_once(')').map_or(rest, |(name, _)| name)),
        None => (body, "[unknown]"),
    };
    let after = body.rsplit_once(") + ").map(|(_, offset)| offset.trim());
    Node {
        depth,
        count,
        symbol: before.trim().to_owned(),
        object: object.trim().to_owned(),
        offset: after.and_then(|offset| offset.parse().ok()),
        thread: false,
    }
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

    const REPORT: &str = "\
Analysis of sampling hello (pid 40) every 1 millisecond
Call graph:
    100 Thread_1001   DispatchQueue_1: com.apple.main-thread  (serial)
      100 start  (in dyld) + 1903  [0x1002b4f28]
        100 _kira_fn_0_main  (in hello) + 52  [0x100001f34]
          80 _kira_fn_4_Grid_step  (in hello) + 120  [0x100002abc]
          20 kira_rt_string_new  (in hello) + 8  [0x100003000]

Total number in stack: 100
";

    #[test]
    fn a_call_tree_expands_into_one_sample_for_each_count_it_owned() {
        let profile = parse(REPORT, 1_000, &symbols());
        assert_eq!(profile.samples.len(), 100);
        let step = profile
            .samples
            .iter()
            .filter(|sample| {
                sample
                    .leaf()
                    .is_some_and(|leaf| profile.frames.symbol_of(leaf) == "Grid.step")
            })
            .count();
        assert_eq!(step, 80);
        let leaf = profile.samples[0].leaf().expect("a leaf");
        assert_eq!(profile.frames.frame(leaf).kind, FrameKind::Kira);
        assert_eq!(profile.thread_name(ThreadId::new(0)), "Thread_1001");
    }

    #[test]
    fn a_frame_line_keeps_the_symbol_the_image_and_the_offset() {
        let node = read_frame(
            4,
            80,
            "_kira_fn_4_Grid_step  (in hello) + 120  [0x100002abc]",
        );
        assert_eq!(node.symbol, "_kira_fn_4_Grid_step");
        assert_eq!(node.object, "hello");
        assert_eq!(node.offset, Some(120));
    }
}
