//! A built, not-yet-running debug target.
//!
//! `kira debug` builds a program and launches a debugger over it in one step.
//! A session that outlives one command needs the two halves separated: the
//! artifacts, the executable that hosts them, and the identities a breakpoint
//! is resolved against, described once and then debugged for as long as the
//! caller wants. That description is this type, and it is the contract between
//! the compiler and any debugger frontend driving it.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::model::{Backend, DebugInfo};

/// The native symbol the VM calls once per interpreted instruction.
pub const VM_PROBE_SYMBOL: &str = "kira_vm_debug_probe";
/// The exported symbol holding the decoded VM state at a probe stop.
pub const VM_TEXT_SYMBOL: &str = "KIRA_VM_DEBUG_TEXT";
/// The exported word that tells the VM whether a debugger still wants stops.
pub const VM_ACTIVE_SYMBOL: &str = "KIRA_VM_DEBUG_ACTIVE";

/// The engine a prepared function's body runs on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Execution {
    /// Interpreted Kira bytecode, reached through the VM probe.
    Bytecode,
    /// Machine code with a native symbol a debugger can break on directly.
    Native,
}

/// One function a breakpoint can name in a prepared target.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreparedFunction {
    /// The function's index in IR/module tables.
    pub id: u32,
    /// The Kira spelling a caller writes in a breakpoint.
    pub name: String,
    /// The native symbol, when this function has a machine-code body.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symbol: Option<String>,
    /// The source line the declaration is on.
    pub line: u32,
    /// Where this function's body executes.
    pub execution: Execution,
}

/// How a debugger reaches VM instruction stops in a prepared target.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VmProbe {
    /// The native symbol to break on.
    pub symbol: String,
    /// The exported symbol carrying decoded state at each stop.
    pub text_symbol: String,
    /// The exported word that turns instruction stops off and on again.
    ///
    /// Defaulted for a target described by an older toolchain, so a frontend
    /// reading one still knows the name to write to.
    #[serde(default = "default_active_symbol")]
    pub active_symbol: String,
    /// The register holding the stopped function's identifier, when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub function_register: Option<String>,
    /// The register holding the stopped instruction index, when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pc_register: Option<String>,
}

impl VmProbe {
    /// The probe for this host, with the registers its calling convention uses.
    #[must_use]
    pub fn host() -> Self {
        let (function_register, pc_register) = match probe_registers() {
            Some((function, pc)) => (Some(function.to_owned()), Some(pc.to_owned())),
            None => (None, None),
        };
        Self {
            symbol: VM_PROBE_SYMBOL.to_owned(),
            text_symbol: VM_TEXT_SYMBOL.to_owned(),
            active_symbol: VM_ACTIVE_SYMBOL.to_owned(),
            function_register,
            pc_register,
        }
    }

    /// An LLDB condition that stops only at `function_id`, instruction `pc`.
    ///
    /// Returns `None` when the host's probe registers are unknown, which is
    /// what tells a caller to stop at every instruction and filter afterwards
    /// rather than install a condition that would never be true.
    #[must_use]
    pub fn condition(&self, function_id: u32, pc: u32) -> Option<String> {
        let function_register = self.function_register.as_deref()?;
        let pc_register = self.pc_register.as_deref()?;
        Some(format!(
            "({function_register} == {function_id} && {pc_register} == {pc})"
        ))
    }
}

/// The stop switch a target described before this field existed still has.
fn default_active_symbol() -> String {
    VM_ACTIVE_SYMBOL.to_owned()
}

/// The registers the VM probe's first two arguments arrive in.
#[must_use]
pub fn probe_registers() -> Option<(&'static str, &'static str)> {
    if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
        Some(("$rcx", "$rdx"))
    } else if cfg!(all(unix, target_arch = "x86_64")) {
        Some(("$rdi", "$rsi"))
    } else if cfg!(target_arch = "aarch64") {
        Some(("$x0", "$x1"))
    } else {
        None
    }
}

/// Everything a debugger needs to start a session over a built program.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreparedTarget {
    /// The backend the program was built for.
    pub backend: String,
    /// The Kira source the identities were recovered from.
    pub source: PathBuf,
    /// The artifact name shown in debugger output.
    pub module_name: String,
    /// The executable a debugger launches.
    pub executable: PathBuf,
    /// The arguments that executable is launched with.
    pub arguments: Vec<String>,
    /// Whether the native unit was built optimized.
    pub optimized: bool,
    /// Every function a breakpoint can name.
    pub functions: Vec<PreparedFunction>,
    /// The VM probe, on backends that interpret bytecode.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub probe: Option<VmProbe>,
    /// Files this target owns, to be removed when the session ends.
    pub artifacts: Vec<PathBuf>,
}

impl PreparedTarget {
    /// Describes a built target from the debug identities the compiler emitted.
    #[must_use]
    pub fn new(info: &DebugInfo, executable: impl Into<PathBuf>) -> Self {
        let functions = info
            .functions
            .iter()
            .map(|function| PreparedFunction {
                id: function.id,
                name: function.name.clone(),
                symbol: function.symbol.clone(),
                line: function.line,
                execution: match function.symbol {
                    Some(_) => Execution::Native,
                    None => Execution::Bytecode,
                },
            })
            .collect();
        Self {
            backend: info.backend.label().to_owned(),
            source: info
                .source
                .as_ref()
                .map(|source| source.path.clone())
                .unwrap_or_default(),
            module_name: info.module_name.clone(),
            executable: executable.into(),
            arguments: Vec::new(),
            optimized: info.optimized,
            functions,
            probe: matches!(info.backend, Backend::Vm | Backend::Hybrid).then(VmProbe::host),
            artifacts: Vec::new(),
        }
    }

    /// Sets the arguments the target executable is launched with.
    #[must_use]
    pub fn with_arguments(mut self, arguments: Vec<String>) -> Self {
        self.arguments = arguments;
        self
    }

    /// Records the files this target owns.
    #[must_use]
    pub fn with_artifacts(mut self, artifacts: Vec<PathBuf>) -> Self {
        self.artifacts = artifacts;
        self
    }

    /// The function a breakpoint spelling names.
    ///
    /// A caller may write the Kira name, the native symbol, or the numeric
    /// identifier, because all three appear in debugger output and a caller
    /// reading one should not have to translate it back.
    #[must_use]
    pub fn function(&self, name: &str) -> Option<&PreparedFunction> {
        self.functions.iter().find(|function| {
            function.name == name
                || function.symbol.as_deref() == Some(name)
                || function.id.to_string() == name
        })
    }

    /// Removes the files this target owns.
    pub fn clean(&self) {
        for artifact in &self.artifacts {
            let _ = std::fs::remove_file(artifact);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{DebugFunction, DebugSource};

    fn info(backend: Backend) -> DebugInfo {
        DebugInfo {
            module_name: "demo".to_owned(),
            backend,
            source: Some(DebugSource {
                path: PathBuf::from("demo.kira"),
            }),
            functions: vec![
                DebugFunction {
                    id: 0,
                    name: "main".to_owned(),
                    backend,
                    symbol: matches!(backend, Backend::Llvm).then(|| "kira_fn_0_main".to_owned()),
                    line: 3,
                },
                DebugFunction {
                    id: 1,
                    name: "step".to_owned(),
                    backend,
                    symbol: None,
                    line: 9,
                },
            ],
            optimized: false,
        }
    }

    #[test]
    fn a_bytecode_backend_carries_the_probe_a_breakpoint_needs() {
        let target = PreparedTarget::new(&info(Backend::Vm), "kira.exe");
        let probe = target.probe.expect("the VM backend has a probe");
        assert_eq!(probe.symbol, VM_PROBE_SYMBOL);
        assert_eq!(probe.text_symbol, VM_TEXT_SYMBOL);
        assert_eq!(target.backend, "vm");
    }

    #[test]
    fn a_native_backend_needs_no_probe_because_it_has_real_symbols() {
        let target = PreparedTarget::new(&info(Backend::Llvm), "demo.exe");
        assert!(target.probe.is_none());
        assert_eq!(
            target.functions[0].symbol.as_deref(),
            Some("kira_fn_0_main")
        );
        assert_eq!(target.functions[0].execution, Execution::Native);
        assert_eq!(target.functions[1].execution, Execution::Bytecode);
    }

    #[test]
    fn a_function_is_found_by_name_symbol_or_identifier() {
        let target = PreparedTarget::new(&info(Backend::Llvm), "demo.exe");
        assert_eq!(target.function("main").map(|function| function.id), Some(0));
        assert_eq!(
            target
                .function("kira_fn_0_main")
                .map(|function| function.id),
            Some(0)
        );
        assert_eq!(target.function("1").map(|function| function.id), Some(1));
        assert!(target.function("absent").is_none());
    }

    #[test]
    fn a_probe_condition_names_both_registers_when_the_host_has_them() {
        let probe = VmProbe {
            symbol: VM_PROBE_SYMBOL.to_owned(),
            text_symbol: VM_TEXT_SYMBOL.to_owned(),
            active_symbol: VM_ACTIVE_SYMBOL.to_owned(),
            function_register: Some("$rcx".to_owned()),
            pc_register: Some("$rdx".to_owned()),
        };
        assert_eq!(
            probe.condition(4, 2).as_deref(),
            Some("($rcx == 4 && $rdx == 2)")
        );
    }

    /// An unknown calling convention must not produce a condition that would
    /// silently never match: the caller stops everywhere instead.
    #[test]
    fn a_host_without_known_probe_registers_has_no_condition() {
        let probe = VmProbe {
            symbol: VM_PROBE_SYMBOL.to_owned(),
            text_symbol: VM_TEXT_SYMBOL.to_owned(),
            active_symbol: VM_ACTIVE_SYMBOL.to_owned(),
            function_register: None,
            pc_register: None,
        };
        assert!(probe.condition(4, 2).is_none());
    }

    #[test]
    fn a_prepared_target_round_trips_through_its_json_contract() {
        let target = PreparedTarget::new(&info(Backend::Vm), "kira.exe")
            .with_arguments(vec!["__vm-debug-host".to_owned()])
            .with_artifacts(vec![PathBuf::from("demo.kbc")]);
        let text = serde_json::to_string(&target).expect("serialize");
        let parsed: PreparedTarget = serde_json::from_str(&text).expect("deserialize");
        assert_eq!(parsed, target);
    }
}
