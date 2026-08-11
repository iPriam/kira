//! LLDB-shaped stepping and breakpoints for the portable VM.

use std::collections::BTreeSet;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use kira_vm_runtime::debug::{VmDebugAction, VmDebugEvent, VmDebugObserver};

use crate::DebugInfo;

/// A VM breakpoint by function name/id and optional instruction index.
///
/// A function-only breakpoint means the function entry (`pc = 0`), matching
/// native debugger behavior. Use `function:pc` to stop at a later instruction.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Breakpoint {
    /// A function id when the caller supplied one, otherwise `None`.
    pub function_id: Option<u32>,
    /// The source-level function name, when known.
    pub function_name: String,
    /// An instruction index, or the function entry when absent.
    pub pc: Option<usize>,
}

impl Breakpoint {
    /// Parses `function` or `function:pc`.
    pub fn parse(value: &str) -> Option<Self> {
        let (function_name, pc) = value.rsplit_once(':').map_or((value, None), |(name, pc)| {
            pc.parse::<usize>()
                .map_or((value, None), |pc| (name, Some(pc)))
        });
        (!function_name.is_empty()).then(|| Self {
            function_id: function_name.parse::<u32>().ok(),
            function_name: function_name.to_owned(),
            pc,
        })
    }

    fn matches(&self, event: &VmDebugEvent<'_>) -> bool {
        let function_matches = self.function_id.is_some_and(|id| id == event.function_id)
            || self.function_name == event.function_name;
        function_matches && self.pc.map_or(event.pc == 0, |pc| pc == event.pc)
    }
}

/// How a VM debugger handles a stop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VmDebuggerMode {
    /// Read commands from stdin when a stop is reached.
    Interactive,
    /// Continue automatically while printing stop locations.
    Batch,
}

/// A debugger attached to a VM run.
pub struct VmDebugger {
    breakpoints: BTreeSet<Breakpoint>,
    mode: VmDebuggerMode,
    pause_on_entry: bool,
    single_step: bool,
    started: bool,
    disassemble_on_stop: bool,
    source_path: Option<PathBuf>,
    source_lines: Vec<String>,
    source_locations: BTreeSet<(u32, u32)>,
}

impl VmDebugger {
    /// Creates a debugger that pauses at the first instruction.
    #[must_use]
    pub fn new(mode: VmDebuggerMode) -> Self {
        Self {
            breakpoints: BTreeSet::new(),
            mode,
            pause_on_entry: true,
            single_step: false,
            started: false,
            disassemble_on_stop: false,
            source_path: None,
            source_lines: Vec::new(),
            source_locations: BTreeSet::new(),
        }
    }

    /// Adds a function or function/instruction breakpoint.
    pub fn add_breakpoint(&mut self, breakpoint: Breakpoint) {
        self.breakpoints.insert(breakpoint);
    }

    /// Adds a breakpoint from CLI spelling.
    pub fn add_breakpoint_text(&mut self, value: &str) -> bool {
        let Some(breakpoint) = Breakpoint::parse(value) else {
            return false;
        };
        self.add_breakpoint(breakpoint);
        true
    }

    /// Enables a short instruction listing whenever execution stops.
    pub fn set_disassemble_on_stop(&mut self, enabled: bool) {
        self.disassemble_on_stop = enabled;
    }

    /// Attaches source lines to the shared function identities.
    ///
    /// The VM event seam stays independent of source files; this adapter loads
    /// the optional text once for a debug session and enriches each stop with
    /// the same function declaration line the LLVM metadata uses.
    pub fn set_source_info(&mut self, info: &DebugInfo) {
        self.source_path = info.source.as_ref().map(|source| source.path.clone());
        self.source_lines = self
            .source_path
            .as_ref()
            .and_then(|path| std::fs::read_to_string(path).ok())
            .map_or_else(Vec::new, |text| text.lines().map(str::to_owned).collect());
        self.source_locations = info
            .functions
            .iter()
            .map(|function| (function.id, function.line))
            .collect();
    }

    /// Attaches source text and function identities without requiring a full
    /// compiler IR. This is used by a hybrid debug host launched by LLDB: the
    /// host has the manifest and bytecode, while the parent compiler process
    /// already owns the IR that produced them.
    pub fn set_source_file(&mut self, path: &Path, functions: &[(u32, &str)]) {
        self.source_path = Some(path.to_path_buf());
        self.source_lines = std::fs::read_to_string(path).map_or_else(
            |_| Vec::new(),
            |text| text.lines().map(str::to_owned).collect(),
        );
        self.source_locations = functions
            .iter()
            .enumerate()
            .map(|(fallback, (id, name))| {
                (
                    *id,
                    source_declaration_line(&self.source_lines, name, fallback),
                )
            })
            .collect();
    }

    /// The installed breakpoints in deterministic order.
    pub fn breakpoints(&self) -> impl Iterator<Item = &Breakpoint> {
        self.breakpoints.iter()
    }

    fn should_stop(&self, event: &VmDebugEvent<'_>) -> bool {
        (!self.started && self.pause_on_entry)
            || self.single_step
            || self
                .breakpoints
                .iter()
                .any(|breakpoint| breakpoint.matches(event))
    }

    fn stop(&mut self, event: VmDebugEvent<'_>) -> VmDebugAction {
        self.started = true;
        self.single_step = false;
        print_stop(
            &event,
            self.source_path.as_deref(),
            &self.source_lines,
            &self.source_locations,
        );
        if self.disassemble_on_stop {
            print_disassembly(&event);
        }
        match self.mode {
            VmDebuggerMode::Batch => VmDebugAction::Continue,
            VmDebuggerMode::Interactive => self.command_loop(&event),
        }
    }

    fn command_loop(&mut self, event: &VmDebugEvent<'_>) -> VmDebugAction {
        loop {
            print!("(kira) ");
            if io::stdout().flush().is_err() {
                return VmDebugAction::Stop;
            }
            let mut line = String::new();
            match io::stdin().read_line(&mut line) {
                Ok(0) => return VmDebugAction::Stop,
                Ok(_) => {}
                Err(_) => return VmDebugAction::Stop,
            }
            let mut parts = line.split_whitespace();
            match parts.next().unwrap_or("continue") {
                "c" | "continue" => return VmDebugAction::Continue,
                "s" | "step" => {
                    self.single_step = true;
                    return VmDebugAction::Continue;
                }
                "b" | "break" => match parts.next().and_then(Breakpoint::parse) {
                    Some(breakpoint) => {
                        println!("breakpoint set on {}", breakpoint.function_name);
                        self.add_breakpoint(breakpoint);
                    }
                    None => println!("usage: break function[:instruction]"),
                },
                "bl" | "breakpoints" => {
                    for breakpoint in &self.breakpoints {
                        println!("breakpoint {}", breakpoint_text(breakpoint));
                    }
                }
                "bt" | "backtrace" => {
                    for (index, frame) in event.backtrace.iter().enumerate() {
                        println!(
                            "frame #{index} {} [function {}] at pc {}",
                            frame.function_name, frame.function_id, frame.pc
                        );
                    }
                }
                "locals" | "l" => print_values("locals", event.locals),
                "stack" | "st" => print_values("stack", event.stack),
                "disassemble" | "dis" => print_disassembly(event),
                "q" | "quit" => return VmDebugAction::Stop,
                "help" | "h" => println!(
                    "commands: continue, step, break function[:instruction], \
                     breakpoints, backtrace, locals, stack, disassemble, quit"
                ),
                other => println!("unknown debugger command `{other}`"),
            }
        }
    }
}

impl VmDebugObserver for VmDebugger {
    fn before_instruction(&mut self, event: VmDebugEvent<'_>) -> VmDebugAction {
        if self.should_stop(&event) {
            self.stop(event)
        } else {
            VmDebugAction::Continue
        }
    }
}

fn print_stop(
    event: &VmDebugEvent<'_>,
    source_path: Option<&Path>,
    source_lines: &[String],
    source_locations: &BTreeSet<(u32, u32)>,
) {
    println!(
        "stopped: {} [function {}] pc={} depth={} stack={} instruction={:?}",
        event.function_name,
        event.function_id,
        event.pc,
        event.call_depth,
        event.stack_depth,
        event.instruction,
    );
    if let Some((_, line)) = source_locations
        .iter()
        .find(|(function_id, _)| *function_id == event.function_id)
        && let Some(path) = source_path
    {
        let text = source_lines
            .get(line.saturating_sub(1) as usize)
            .map_or("", String::as_str);
        println!("source: {}:{line} | {text}", path.display());
    }
}

fn print_disassembly(event: &VmDebugEvent<'_>) {
    let start = event.pc.saturating_sub(3);
    let end = (event.pc + 4).min(event.code.len());
    for (pc, instruction) in event.code.iter().enumerate().skip(start).take(end - start) {
        let marker = if pc == event.pc { '>' } else { ' ' };
        println!("{marker} {pc:04} {instruction:?}");
    }
}

fn print_values(label: &str, values: &[kira_vm_runtime::Value]) {
    println!("{label}:");
    if values.is_empty() {
        println!("  <empty>");
        return;
    }
    for (index, value) in values.iter().enumerate() {
        println!("  [{index}] {value:?}");
    }
}

fn breakpoint_text(breakpoint: &Breakpoint) -> String {
    breakpoint.pc.map_or_else(
        || breakpoint.function_name.clone(),
        |pc| format!("{}:{pc}", breakpoint.function_name),
    )
}

fn source_declaration_line(lines: &[String], name: &str, fallback: usize) -> u32 {
    lines
        .iter()
        .position(|line| {
            line.split(|character: char| !character.is_ascii_alphanumeric() && character != '_')
                .any(|word| word == name)
        })
        .map_or(fallback.saturating_add(1) as u32, |line| line as u32 + 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn breakpoints_accept_function_ids_with_optional_instruction_indices() {
        let function = Breakpoint::parse("main").expect("function breakpoint");
        assert_eq!(function.function_id, None);
        assert_eq!(function.function_name, "main");
        assert_eq!(function.pc, None);

        let entry = Breakpoint::parse("7").expect("numeric function breakpoint");
        assert_eq!(entry.function_id, Some(7));
        assert_eq!(entry.pc, None);

        let instruction = Breakpoint::parse("7:12").expect("numeric instruction breakpoint");
        assert_eq!(instruction.function_id, Some(7));
        assert_eq!(instruction.pc, Some(12));
    }
}
