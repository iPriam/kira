//! Instruction profiling for Kira run sessions.
//!
//! Layer 8 of the Kira package graph. The VM owns the observer seam; this
//! crate owns the accounting and presentation so a debugger, CLI command, or
//! embedding application can ask the same question: where did the interpreter
//! spend its instructions?

use std::collections::BTreeMap;
use std::fmt::Write as _;

use kira_vm_runtime::debug::{VmDebugAction, VmDebugEvent, VmDebugObserver};

/// One instruction location and the number of times it executed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstructionSite {
    /// The function containing the instruction.
    pub function_id: u32,
    /// The function's source-level name.
    pub function_name: String,
    /// The bytecode program counter.
    pub pc: usize,
    /// Number of visits to this location.
    pub hits: u64,
}

/// One function's aggregate instruction count.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FunctionProfile {
    /// The function index in the bytecode module.
    pub function_id: u32,
    /// The source-level function name.
    pub name: String,
    /// Total instruction visits in this function.
    pub instructions: u64,
    /// The hottest instruction locations, sorted by execution count.
    pub sites: Vec<InstructionSite>,
}

/// A completed VM instruction profile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileReport {
    /// Total instructions observed across all functions.
    pub total_instructions: u64,
    /// Functions sorted hottest first.
    pub functions: Vec<FunctionProfile>,
}

#[derive(Debug, Default)]
struct FunctionAccumulator {
    name: String,
    instructions: u64,
    sites: BTreeMap<usize, u64>,
}

/// A zero-allocation-at-the-call-site VM profiler.
///
/// The observer receives borrowed event data. Names are copied only on the
/// first instruction seen for a function, while the hot path updates integer
/// counters in maps owned by the profiler.
#[derive(Debug, Default)]
pub struct VmProfiler {
    total_instructions: u64,
    functions: BTreeMap<u32, FunctionAccumulator>,
}

impl VmProfiler {
    /// Creates an empty profiler.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Records one event. Public for embedders that already have an event
    /// stream but do not want to install the observer directly.
    pub fn record(&mut self, function_id: u32, function_name: &str, pc: usize) {
        self.total_instructions = self.total_instructions.saturating_add(1);
        let function = self
            .functions
            .entry(function_id)
            .or_insert_with(|| FunctionAccumulator {
                name: function_name.to_owned(),
                ..FunctionAccumulator::default()
            });
        function.instructions = function.instructions.saturating_add(1);
        let site = function.sites.entry(pc).or_default();
        *site = site.saturating_add(1);
    }

    /// Finishes the profile, sorting the useful rows hottest first.
    #[must_use]
    pub fn finish(self) -> ProfileReport {
        let mut functions = self
            .functions
            .into_iter()
            .map(|(function_id, function)| {
                let mut sites = function
                    .sites
                    .into_iter()
                    .map(|(pc, hits)| InstructionSite {
                        function_id,
                        function_name: function.name.clone(),
                        pc,
                        hits,
                    })
                    .collect::<Vec<_>>();
                sites.sort_by(|left, right| {
                    right
                        .hits
                        .cmp(&left.hits)
                        .then_with(|| left.pc.cmp(&right.pc))
                });
                FunctionProfile {
                    function_id,
                    name: function.name,
                    instructions: function.instructions,
                    sites,
                }
            })
            .collect::<Vec<_>>();
        functions.sort_by(|left, right| {
            right
                .instructions
                .cmp(&left.instructions)
                .then_with(|| left.function_id.cmp(&right.function_id))
        });
        ProfileReport {
            total_instructions: self.total_instructions,
            functions,
        }
    }
}

impl VmDebugObserver for VmProfiler {
    fn before_instruction(&mut self, event: VmDebugEvent<'_>) -> VmDebugAction {
        self.record(event.function_id, event.function_name, event.pc);
        VmDebugAction::Continue
    }
}

/// Renders a compact, stable report for terminals and CI logs.
#[must_use]
pub fn render_text(report: &ProfileReport, max_functions: usize, max_sites: usize) -> String {
    let mut output = String::new();
    let _ = writeln!(
        output,
        "instructions: {} across {} function(s)",
        report.total_instructions,
        report.functions.len()
    );
    for function in report.functions.iter().take(max_functions) {
        let share = if report.total_instructions == 0 {
            0.0
        } else {
            100.0 * function.instructions as f64 / report.total_instructions as f64
        };
        let _ = writeln!(
            output,
            "  {:>10}  {:>5.1}%  {} (#{})",
            function.instructions, share, function.name, function.function_id
        );
        for site in function.sites.iter().take(max_sites) {
            let _ = writeln!(output, "    pc {:>5}  {:>10}", site.pc, site.hits);
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profiles_are_sorted_by_hotness_and_keep_instruction_sites() {
        let mut profiler = VmProfiler::new();
        profiler.record(1, "cold", 3);
        profiler.record(0, "main", 2);
        profiler.record(0, "main", 2);
        profiler.record(0, "main", 1);
        let report = profiler.finish();
        assert_eq!(report.total_instructions, 4);
        assert_eq!(report.functions[0].name, "main");
        assert_eq!(report.functions[0].sites[0].pc, 2);
        assert_eq!(report.functions[0].sites[0].hits, 2);
    }

    #[test]
    fn text_output_is_machine_readable_enough_for_ci_logs() {
        let mut profiler = VmProfiler::new();
        profiler.record(0, "main", 4);
        let text = render_text(&profiler.finish(), 10, 10);
        assert!(text.contains("instructions: 1 across 1 function(s)"));
        assert!(text.contains("pc     4"), "{text}");
    }
}
