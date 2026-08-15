//! The in-process sampler that reads what the interpreter publishes.
//!
//! A machine sampler can see that the process is inside the interpreter; it
//! cannot see which Kira function the interpreter is interpreting, because that
//! lives in the VM's own frame stack rather than on the machine stack. This
//! sampler reads the Kira stack the VM publishes
//! ([`kira_vm_runtime::profile`]) on the same period, and is what gives a VM or
//! hybrid run the Kira view an LLVM run gets from its machine frames.
//!
//! A thread with nothing published is not running Kira code — it is compiling,
//! linking, or waiting — so it is not sampled. That is what keeps a recording
//! about the program rather than about the command that started it.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Instant;

use kira_vm_runtime::profile::{ShadowFrame, ThreadTag};

use crate::clock::Ticker;
use crate::model::{Frame, FrameKind, Nanos, Profile, Sample, ThreadId, ThreadRecord, View};
use crate::symbols::KiraSymbols;

/// One read of one thread's published Kira stack.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeSample {
    /// The thread the stack belongs to.
    pub thread: ThreadTag,
    /// When the read happened, from the recording's origin.
    pub time: Nanos,
    /// The stack, outermost frame first.
    pub frames: Vec<ShadowFrame>,
    /// Frames too deep to publish, which a report shows as an elision.
    pub omitted: u32,
}

/// Everything one sampling run read.
#[derive(Debug, Default)]
pub struct RuntimeSamples {
    /// Every read, in the order it happened.
    pub samples: Vec<RuntimeSample>,
    /// The threads that were seen running Kira code.
    pub threads: Vec<(ThreadTag, String)>,
    /// Reads abandoned because the interpreter kept changing the stack.
    pub lost: u64,
    /// The frequency the sampler was asked for.
    pub frequency: u32,
}

/// A running Kira-level sampler.
#[derive(Debug)]
pub struct RuntimeSampler {
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<RuntimeSamples>>,
}

impl RuntimeSampler {
    /// Starts sampling every thread that runs Kira code, timing from `origin`.
    ///
    /// Publication itself is turned on here, so a caller cannot start a sampler
    /// that reads a stack nothing writes.
    #[must_use]
    pub fn start(frequency: u32, origin: Instant) -> Self {
        kira_vm_runtime::profile::set_enabled(true);
        let stop = Arc::new(AtomicBool::new(false));
        let signal = Arc::clone(&stop);
        let worker = std::thread::Builder::new()
            .name("kira-profile-sampler".to_owned())
            .spawn(move || sample_until(&signal, frequency, origin))
            .ok();
        Self { stop, worker }
    }

    /// Stops sampling and takes what was read.
    ///
    /// Publication is turned off before the sampler is joined, so no run that
    /// starts afterwards pays for a recording that has ended.
    #[must_use]
    pub fn finish(mut self) -> RuntimeSamples {
        kira_vm_runtime::profile::set_enabled(false);
        self.stop.store(true, Ordering::Relaxed);
        match self.worker.take() {
            Some(worker) => worker.join().unwrap_or_default(),
            None => RuntimeSamples {
                frequency: 0,
                ..RuntimeSamples::default()
            },
        }
    }
}

fn sample_until(stop: &AtomicBool, frequency: u32, origin: Instant) -> RuntimeSamples {
    let mut ticker = Ticker::new(frequency);
    let mut collected = RuntimeSamples {
        frequency,
        ..RuntimeSamples::default()
    };
    let mut frames = Vec::with_capacity(64);
    while !stop.load(Ordering::Relaxed) {
        ticker.wait();
        let time = Nanos::new(origin.elapsed().as_nanos().min(u128::from(u64::MAX)) as u64);
        for stack in kira_vm_runtime::profile::live_stacks() {
            if stack.depth() == 0 {
                continue;
            }
            match stack.snapshot(&mut frames) {
                Some(omitted) => {
                    if frames.is_empty() {
                        continue;
                    }
                    let tag = stack.tag();
                    if !collected.threads.iter().any(|(known, _)| *known == tag) {
                        collected.threads.push((tag, stack.name().to_owned()));
                    }
                    collected.samples.push(RuntimeSample {
                        thread: tag,
                        time,
                        frames: frames.clone(),
                        omitted,
                    });
                }
                None => collected.lost = collected.lost.saturating_add(1),
            }
        }
    }
    collected
}

impl RuntimeSamples {
    /// Turns the reads into the Kira view of the run.
    ///
    /// A sample's weight is the time since the previous sample of the same
    /// thread, so the view accounts for exactly the wall-clock time that thread
    /// spent inside Kira code — including a sampler that fell behind, whose
    /// remaining samples each carry the interval they actually covered.
    #[must_use]
    pub fn into_profile(self, symbols: &KiraSymbols) -> Profile {
        let mut profile = Profile::new(View::Kira, "kira-wall", "kira-runtime");
        profile.frequency = self.frequency;
        profile.lost = self.lost;
        for (index, (_, name)) in self.threads.iter().enumerate() {
            profile.threads.push(ThreadRecord {
                id: ThreadId::new(index as u32),
                name: name.clone(),
            });
        }
        let source = symbols
            .source()
            .map(|path| path.to_string_lossy().into_owned());
        let file = source.map(|path| profile.frames.name(&path));
        let object = profile.frames.name(INTERPRETED_OBJECT);
        let elided = profile.frames.name("[frames too deep to publish]");

        let period = Nanos::new(1_000_000_000 / u64::from(self.frequency.max(1)));
        let mut previous: Vec<Option<Nanos>> = vec![None; self.threads.len()];
        for read in self.samples {
            let Some(thread) = self
                .threads
                .iter()
                .position(|(tag, _)| *tag == read.thread)
                .map(|index| ThreadId::new(index as u32))
            else {
                continue;
            };
            let last = previous.get(thread.index() as usize).copied().flatten();
            let weight = last.map_or(period, |last| last.until(read.time)).get();
            if let Some(slot) = previous.get_mut(thread.index() as usize) {
                *slot = Some(read.time);
            }
            // The published stack keeps its outermost frames and its innermost
            // one; the elision names the frames between them that a deep run
            // had no slot for.
            let mut stack = Vec::with_capacity(read.frames.len() + 1);
            let elide_before = read.omitted > 0;
            let last = read.frames.len().saturating_sub(1);
            for (index, entry) in read.frames.iter().enumerate() {
                if elide_before && index == last {
                    stack.push(profile.frames.insert(Frame::named(
                        elided,
                        FrameKind::Unknown,
                        object,
                    )));
                }
                stack.push(kira_frame(&mut profile, symbols, entry, object, file));
            }
            profile.samples.push(Sample {
                thread,
                time: read.time,
                weight,
                stack,
            });
        }
        profile
    }
}

/// The image a report names for code the interpreter was running.
///
/// Interpreted code is in no image: it is bytecode inside the process that is
/// interpreting it. Naming it keeps the shared-object column meaningful across
/// backends instead of blank for half of them.
pub const INTERPRETED_OBJECT: &str = "[vm]";

fn kira_frame(
    profile: &mut Profile,
    symbols: &KiraSymbols,
    entry: &ShadowFrame,
    object: kira_core::Symbol,
    file: Option<kira_core::Symbol>,
) -> crate::model::FrameId {
    let identity = symbols.function(entry.function);
    let name = match identity {
        Some(identity) => profile.frames.name(&identity.name),
        None => profile.frames.name(&format!("function-{}", entry.function)),
    };
    let frame = Frame {
        symbol: name,
        kind: FrameKind::Kira,
        object,
        function: Some(entry.function),
        offset: Some(entry.pc),
        file,
        line: identity.map(|identity| identity.line),
    };
    profile.frames.insert(frame)
}

#[cfg(test)]
mod tests {
    use super::*;
    use kira_debug::{Backend, DebugFunction, DebugInfo};

    fn symbols() -> KiraSymbols {
        KiraSymbols::from_debug(&DebugInfo {
            module_name: "hello".to_owned(),
            backend: Backend::Vm,
            source: None,
            functions: vec![
                DebugFunction {
                    id: 0,
                    name: "main".to_owned(),
                    backend: Backend::Vm,
                    symbol: None,
                    line: 1,
                },
                DebugFunction {
                    id: 1,
                    name: "Grid.step".to_owned(),
                    backend: Backend::Vm,
                    symbol: None,
                    line: 9,
                },
            ],
            optimized: false,
        })
    }

    fn read(time: u64, frames: Vec<ShadowFrame>) -> RuntimeSample {
        RuntimeSample {
            thread: ThreadTag::new(0),
            time: Nanos::new(time),
            frames,
            omitted: 0,
        }
    }

    #[test]
    fn a_samples_weight_is_the_interval_since_the_threads_previous_sample() {
        let tag = ThreadTag::new(0);
        let samples = RuntimeSamples {
            samples: vec![
                read(1_000_000, vec![ShadowFrame { function: 0, pc: 2 }]),
                read(
                    3_000_000,
                    vec![
                        ShadowFrame { function: 0, pc: 4 },
                        ShadowFrame { function: 1, pc: 7 },
                    ],
                ),
            ],
            threads: vec![(tag, "main".to_owned())],
            lost: 1,
            frequency: 1_000,
        };
        let profile = samples.into_profile(&symbols());

        assert_eq!(profile.samples.len(), 2);
        assert_eq!(profile.samples[0].weight, 1_000_000);
        assert_eq!(profile.samples[1].weight, 2_000_000);
        assert_eq!(profile.lost, 1);
        let leaf = profile.samples[1].stack[1];
        assert_eq!(profile.frames.symbol_of(leaf), "Grid.step");
        assert_eq!(profile.frames.frame(leaf).offset, Some(7));
    }

    #[test]
    fn frames_a_deep_run_could_not_publish_are_elided_before_the_innermost_one() {
        let tag = ThreadTag::new(0);
        let samples = RuntimeSamples {
            samples: vec![RuntimeSample {
                thread: tag,
                time: Nanos::new(1_000),
                frames: vec![
                    ShadowFrame { function: 0, pc: 1 },
                    ShadowFrame { function: 1, pc: 0 },
                ],
                omitted: 3,
            }],
            threads: vec![(tag, "main".to_owned())],
            lost: 0,
            frequency: 1_000,
        };
        let profile = samples.into_profile(&symbols());
        let stack = &profile.samples[0].stack;
        assert_eq!(profile.frames.symbol_of(stack[0]), "main");
        assert!(
            profile
                .frames
                .symbol_of(stack[1])
                .contains("too deep to publish")
        );
        assert_eq!(profile.frames.symbol_of(stack[2]), "Grid.step");
    }
}
