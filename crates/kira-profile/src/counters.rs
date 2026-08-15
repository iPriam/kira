//! Exact instruction counting for a VM run.
//!
//! Sampling answers "where is the time going" statistically. This answers
//! "what did the interpreter actually execute" exactly, by observing every
//! instruction — which is the `-e instructions` event, and the only event whose
//! numbers are counts rather than estimates.
//!
//! It costs what it sounds like it costs: a callback per interpreted
//! instruction. Nobody should reach for it to find a hot function, and everyone
//! should reach for it to find out why a loop runs three times more often than
//! it should. The counts are aggregated per instruction site rather than
//! streamed, so a run of any length produces a profile the size of the program.

use std::collections::HashMap;

use kira_vm_runtime::debug::{VmDebugAction, VmDebugEvent, VmDebugObserver};

use crate::model::{Frame, FrameKind, Nanos, Profile, Sample, ThreadId, ThreadRecord, Unit, View};
use crate::runtime::INTERPRETED_OBJECT;
use crate::symbols::KiraSymbols;

/// Counts every instruction the interpreter executes.
#[derive(Debug, Default)]
pub struct InstructionCounter {
    sites: HashMap<(u32, u32), u64>,
    total: u64,
}

impl InstructionCounter {
    /// A counter that has seen nothing.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Records one executed instruction.
    ///
    /// Public because an embedder that already has an instruction stream can
    /// count into this without installing an observer.
    pub fn record(&mut self, function: u32, pc: u32) {
        self.total = self.total.saturating_add(1);
        let site = self.sites.entry((function, pc)).or_default();
        *site = site.saturating_add(1);
    }

    /// The finished counts, hottest site first.
    #[must_use]
    pub fn finish(self) -> InstructionProfile {
        let mut sites = self
            .sites
            .into_iter()
            .map(|((function, pc), count)| InstructionSite {
                function,
                pc,
                count,
            })
            .collect::<Vec<_>>();
        sites.sort_by(|left, right| {
            right
                .count
                .cmp(&left.count)
                .then_with(|| left.function.cmp(&right.function))
                .then_with(|| left.pc.cmp(&right.pc))
        });
        InstructionProfile {
            total: self.total,
            sites,
        }
    }
}

impl VmDebugObserver for InstructionCounter {
    fn before_instruction(&mut self, event: VmDebugEvent<'_>) -> VmDebugAction {
        self.record(event.function_id, event.pc.min(u32::MAX as usize) as u32);
        VmDebugAction::Continue
    }
}

/// One instruction location and how many times it executed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InstructionSite {
    /// The function containing the instruction.
    pub function: u32,
    /// The bytecode instruction index.
    pub pc: u32,
    /// How many times it executed.
    pub count: u64,
}

/// A completed instruction count.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InstructionProfile {
    /// Instructions executed across the whole run.
    pub total: u64,
    /// Every instruction site, hottest first.
    pub sites: Vec<InstructionSite>,
}

impl InstructionProfile {
    /// Turns the counts into a profile the ordinary reports render.
    ///
    /// One sample per site, weighted by its count: an exact profile has no
    /// stacks to carry, because a count is attached to a place in the program
    /// rather than to a moment in the run.
    #[must_use]
    pub fn into_profile(self, symbols: &KiraSymbols) -> Profile {
        let mut profile = Profile::new(View::Kira, "instructions", "kira-vm");
        profile.unit = Unit::Instructions;
        profile.threads.push(ThreadRecord {
            id: ThreadId::new(0),
            name: "main".to_owned(),
        });
        let object = profile.frames.name(INTERPRETED_OBJECT);
        let file = symbols
            .source()
            .map(|path| profile.frames.name(&path.to_string_lossy()));
        for site in self.sites {
            let identity = symbols.function(site.function);
            let name = match identity {
                Some(identity) => profile.frames.name(&identity.name),
                None => profile.frames.name(&format!("function-{}", site.function)),
            };
            let frame = profile.frames.insert(Frame {
                symbol: name,
                kind: FrameKind::Kira,
                object,
                function: Some(site.function),
                offset: Some(site.pc),
                file,
                line: identity.map(|identity| identity.line),
            });
            profile.samples.push(Sample {
                thread: ThreadId::new(0),
                time: Nanos::ZERO,
                weight: site.count,
                stack: vec![frame],
            });
        }
        profile
    }
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
            functions: vec![DebugFunction {
                id: 0,
                name: "main".to_owned(),
                backend: Backend::Vm,
                symbol: None,
                line: 1,
            }],
            optimized: false,
        })
    }

    #[test]
    fn counts_are_exact_and_ordered_hottest_first() {
        let mut counter = InstructionCounter::new();
        counter.record(0, 2);
        counter.record(0, 2);
        counter.record(0, 1);
        counter.record(1, 5);
        let counted = counter.finish();

        assert_eq!(counted.total, 4);
        assert_eq!(counted.sites[0].pc, 2);
        assert_eq!(counted.sites[0].count, 2);
    }

    #[test]
    fn a_count_becomes_one_weighted_sample_for_each_instruction_site() {
        let mut counter = InstructionCounter::new();
        counter.record(0, 3);
        counter.record(0, 3);
        let profile = counter.finish().into_profile(&symbols());

        assert_eq!(profile.unit, Unit::Instructions);
        assert_eq!(profile.total(), 2);
        assert_eq!(profile.samples.len(), 1);
        let frame = profile.samples[0].stack[0];
        assert_eq!(profile.frames.symbol_of(frame), "main");
        assert_eq!(profile.frames.frame(frame).offset, Some(3));
    }
}
