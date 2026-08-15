//! One live debug session: a built target and the LLDB process debugging it.
//!
//! Breakpoints are kept here rather than left with the adapter. DAP replaces
//! the whole function-breakpoint set on every request, and a VM breakpoint is
//! not a symbol at all — it is a condition on the one probe symbol the
//! interpreter calls. Both need the session to know the full set it is asking
//! for, so both are recorded and re-sent together.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::time::Duration;

use kira_debug::{
    DEFAULT_TIMEOUT, DapClient, Engine, PreparedTarget, Stop, TargetState, VmStop, decode_base64,
    parse_address,
};
use serde::Serialize;
use serde_json::{Value, json};

/// How long a launched target may take to report its first stop.
pub const LAUNCH_TIMEOUT: Duration = Duration::from_secs(180);
/// How much of the VM's published state text is read at a stop.
const TEXT_READ_LIMIT: usize = 4096;

/// Where a breakpoint was placed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Placement {
    /// A Kira bytecode location, reached through the VM probe's condition.
    Kira {
        /// The Kira function name.
        function: String,
        /// The function's identifier in module tables.
        function_id: u32,
        /// The instruction index within that function.
        pc: u32,
    },
    /// A native symbol LLDB breaks on directly.
    Function {
        /// The symbol name.
        symbol: String,
    },
    /// A source file and line.
    Source {
        /// The file the line is in.
        path: PathBuf,
        /// The one-based line number.
        line: u32,
    },
}

impl Placement {
    /// The spelling a report shows for this placement.
    #[must_use]
    pub fn location(&self) -> String {
        match self {
            Self::Kira { function, pc, .. } => format!("{function}:{pc}"),
            Self::Function { symbol } => symbol.clone(),
            Self::Source { path, line } => format!("{}:{line}", path.display()),
        }
    }
}

/// One breakpoint this session owns.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Breakpoint {
    /// The session-assigned identifier a caller deletes by.
    pub id: u32,
    /// Where it was placed, in the spelling the caller wrote.
    pub location: String,
    /// Where it was placed.
    #[serde(flatten)]
    pub placement: Placement,
    /// The caller's extra LLDB condition, when one was given.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub condition: Option<String>,
    /// Whether the adapter resolved it to an address.
    pub verified: bool,
    /// The address it resolved to.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub address: Option<String>,
}

/// A built target and the debugger session running it.
pub struct Session {
    /// The identifier every tool call names this session by.
    pub id: String,
    /// What was built, and the identities breakpoints resolve against.
    pub target: PreparedTarget,
    client: DapClient,
    breakpoints: Vec<Breakpoint>,
    next_breakpoint: u32,
    /// Every file a source breakpoint has been placed in, including files whose
    /// breakpoints have since been removed: the adapter replaces one file's
    /// list per request, so a file it was told about must keep being told.
    touched_sources: BTreeSet<PathBuf>,
    stepping: bool,
}

impl Session {
    /// Launches `target` under a debug adapter and configures it.
    pub fn start(id: String, target: PreparedTarget) -> Result<Self, String> {
        let mut client = DapClient::start(Engine::DebugAdapter).map_err(|error| {
            format!("{error}; set KIRA_LLDB_DAP to the `lldb-dap` executable to use another one")
        })?;
        let capabilities = client
            .request(
                "initialize",
                json!({
                    "clientID": "kira-lldb-mcp",
                    "clientName": "Kira LLDB",
                    "adapterID": "lldb-dap",
                    "pathFormat": "path",
                    "linesStartAt1": true,
                    "columnsStartAt1": true,
                    "supportsVariableType": true,
                    "supportsMemoryReferences": true,
                }),
                DEFAULT_TIMEOUT,
            )
            .map_err(|error| error.to_string())?;
        client.set_capabilities(capabilities);
        client
            .await_configuration(
                "launch",
                json!({
                    "program": target.executable,
                    "args": target.arguments,
                    "cwd": std::env::current_dir().unwrap_or_default(),
                    "stopAtEntry": false,
                    "noDebug": false,
                }),
                LAUNCH_TIMEOUT,
            )
            .map_err(|error| error.to_string())?;
        let mut session = Self {
            id,
            target,
            client,
            breakpoints: Vec::new(),
            next_breakpoint: 1,
            touched_sources: BTreeSet::new(),
            stepping: false,
        };
        session.send_breakpoints()?;
        session
            .client
            .request("configurationDone", json!({}), DEFAULT_TIMEOUT)
            .map_err(|error| error.to_string())?;
        Ok(session)
    }

    /// The adapter connection, for a tool that speaks DAP directly.
    pub fn client(&mut self) -> &mut DapClient {
        &mut self.client
    }

    /// What the target is doing.
    pub fn state(&self) -> &TargetState {
        self.client.state()
    }

    /// Every breakpoint this session holds.
    pub fn breakpoints(&self) -> &[Breakpoint] {
        &self.breakpoints
    }

    /// Adds a breakpoint and installs the new set.
    pub fn add_breakpoint(
        &mut self,
        placement: Placement,
        condition: Option<String>,
    ) -> Result<Breakpoint, String> {
        if let Placement::Source { path, .. } = &placement {
            self.touched_sources.insert(path.clone());
        }
        let breakpoint = Breakpoint {
            id: self.next_breakpoint,
            location: placement.location(),
            placement,
            condition,
            verified: false,
            address: None,
        };
        self.next_breakpoint += 1;
        let id = breakpoint.id;
        self.breakpoints.push(breakpoint);
        self.send_breakpoints()?;
        // A session that was told to run free gets its stops back the moment
        // something asks to stop again.
        self.set_vm_stops(true)?;
        self.breakpoints
            .iter()
            .find(|breakpoint| breakpoint.id == id)
            .cloned()
            .ok_or_else(|| format!("breakpoint {id} was lost while being installed"))
    }

    /// Removes breakpoints, returning how many went away.
    pub fn remove_breakpoints(&mut self, ids: Option<&[u32]>) -> Result<usize, String> {
        let before = self.breakpoints.len();
        match ids {
            Some(ids) => self
                .breakpoints
                .retain(|breakpoint| !ids.contains(&breakpoint.id)),
            None => self.breakpoints.clear(),
        }
        let removed = before - self.breakpoints.len();
        self.send_breakpoints()?;
        Ok(removed)
    }

    /// Installs the current breakpoint set with the adapter.
    ///
    /// Every request replaces a whole category, so all three are sent: the
    /// function breakpoints, the probe that carries the Kira conditions, and
    /// one source-breakpoint request per file that still has a line in it. A
    /// file whose last breakpoint was removed is sent an empty list, because
    /// otherwise the adapter would keep what it was last told.
    fn send_breakpoints(&mut self) -> Result<(), String> {
        let (functions, sources) = self.requests();
        let reply = self
            .client
            .request(
                "setFunctionBreakpoints",
                json!({ "breakpoints": functions }),
                DEFAULT_TIMEOUT,
            )
            .map_err(|error| error.to_string())?;
        self.record_function_resolutions(&reply);

        for (path, lines) in sources {
            let reply = self
                .client
                .request(
                    "setBreakpoints",
                    json!({
                        "source": { "path": path },
                        "breakpoints": lines,
                    }),
                    DEFAULT_TIMEOUT,
                )
                .map_err(|error| error.to_string())?;
            self.record_source_resolutions(&path, &reply);
        }
        Ok(())
    }

    /// The breakpoint requests the current set turns into.
    ///
    /// Built before anything is sent, so the borrow of the recorded set ends
    /// before the replies are written back onto it.
    fn requests(&self) -> (Vec<Value>, Vec<(PathBuf, Vec<Value>)>) {
        let mut functions = Vec::new();
        let mut sources: BTreeMap<PathBuf, Vec<Value>> = BTreeMap::new();
        let mut kira = Vec::new();
        for breakpoint in &self.breakpoints {
            match &breakpoint.placement {
                Placement::Function { symbol } => functions.push(json!({
                    "name": symbol,
                    "condition": breakpoint.condition,
                })),
                Placement::Source { path, line } => {
                    // A file keeps its entry after its last line is removed, so
                    // the adapter is told the list is empty rather than left
                    // holding what it was last given.
                    sources
                        .entry(path.clone())
                        .or_default()
                        .push(json!({ "line": line, "condition": breakpoint.condition }));
                }
                Placement::Kira { .. } => kira.push(breakpoint),
            }
        }
        for path in self.source_files() {
            sources.entry(path).or_default();
        }
        functions.extend(probe_request(self.stepping, &kira, &self.target));
        (functions, sources.into_iter().collect())
    }

    /// Frees the probe from its conditions for the duration of a step.
    pub fn begin_step(&mut self) -> Result<(), String> {
        if self.stepping || self.target.probe.is_none() {
            return Ok(());
        }
        self.stepping = true;
        self.send_breakpoints()?;
        self.set_vm_stops(true)
    }

    /// Restores the conditions a step suspended.
    pub fn end_step(&mut self) -> Result<(), String> {
        if !self.stepping {
            return Ok(());
        }
        self.stepping = false;
        match self.client.state().is_alive() {
            true => self.send_breakpoints(),
            // A target that has ended has no adapter left to configure, and
            // saying so here would replace the step's own outcome.
            false => Ok(()),
        }
    }

    /// Every file this session has ever placed a source breakpoint in.
    fn source_files(&self) -> Vec<PathBuf> {
        self.touched_sources.iter().cloned().collect()
    }

    /// Records where the adapter resolved each function breakpoint.
    fn record_function_resolutions(&mut self, reply: &Value) {
        let Some(resolved) = reply["breakpoints"].as_array() else {
            return;
        };
        let mut index = 0;
        for breakpoint in &mut self.breakpoints {
            if !matches!(breakpoint.placement, Placement::Function { .. }) {
                continue;
            }
            apply_resolution(breakpoint, resolved.get(index));
            index += 1;
        }
        // The probe's own entry is last and belongs to every Kira breakpoint:
        // they all stop at the same symbol and differ only by condition.
        let probe = resolved.get(index).cloned().unwrap_or(Value::Null);
        for breakpoint in &mut self.breakpoints {
            if matches!(breakpoint.placement, Placement::Kira { .. }) {
                apply_resolution(breakpoint, Some(&probe));
            }
        }
    }

    /// Records where the adapter resolved one file's source breakpoints.
    fn record_source_resolutions(&mut self, path: &PathBuf, reply: &Value) {
        let Some(resolved) = reply["breakpoints"].as_array() else {
            return;
        };
        let mut index = 0;
        for breakpoint in &mut self.breakpoints {
            match &breakpoint.placement {
                Placement::Source { path: owner, .. } if owner == path => {
                    apply_resolution(breakpoint, resolved.get(index));
                    index += 1;
                }
                _ => {}
            }
        }
    }

    /// Resumes the target and waits for its next stop.
    pub fn resume(&mut self, timeout: Duration) -> Result<Option<Stop>, String> {
        let thread_id = match self.client.state().stop() {
            Some(stop) => stop.thread_id,
            None => return Err(format!("the target is {}", self.client.state().label())),
        };
        self.client
            .request(
                "continue",
                json!({ "threadId": thread_id }),
                DEFAULT_TIMEOUT,
            )
            .map_err(|error| error.to_string())?;
        self.client.mark_running();
        self.await_stop(timeout)
    }

    /// Waits for the next stop, reporting `None` when the target ended instead.
    pub fn await_stop(&mut self, timeout: Duration) -> Result<Option<Stop>, String> {
        match self.client.wait_for_stop(timeout) {
            Ok(stop) => Ok(Some(stop)),
            Err(error) => match self.client.state().is_alive() {
                true => Err(error.to_string()),
                false => Ok(None),
            },
        }
    }

    /// Reads the decoded Kira state a stopped VM published.
    ///
    /// The state is read out of the process's memory rather than by calling
    /// into it: evaluating a target function at a stop is what some LLDB
    /// builds abort on, and a debugger that crashes the program it stopped is
    /// worse than one that reports less.
    pub fn vm_stop(&mut self) -> Result<Option<VmStop>, String> {
        let Some(probe) = self.target.probe.clone() else {
            return Ok(None);
        };
        if self.client.state().stop().is_none() {
            return Ok(None);
        }
        let frame_id = self.top_frame_id()?;
        let evaluation = self
            .client
            .request(
                "evaluate",
                json!({
                    "expression": format!("&{}", probe.text_symbol),
                    "frameId": frame_id,
                    "context": "repl",
                }),
                DEFAULT_TIMEOUT,
            )
            .map_err(|error| error.to_string())?;
        let Some(address) = evaluation["result"].as_str().and_then(parse_address) else {
            return Ok(None);
        };
        let memory = self
            .client
            .request(
                "readMemory",
                json!({ "memoryReference": address, "offset": 0, "count": TEXT_READ_LIMIT }),
                DEFAULT_TIMEOUT,
            )
            .map_err(|error| error.to_string())?;
        let Some(data) = memory["data"].as_str() else {
            return Ok(None);
        };
        let bytes = decode_base64(data);
        let length = bytes
            .iter()
            .position(|byte| *byte == 0)
            .unwrap_or(bytes.len());
        Ok(VmStop::parse(&String::from_utf8_lossy(&bytes[..length])))
    }

    /// Tells the debugged VM whether a debugger still wants instruction stops.
    ///
    /// A VM that has stopped once keeps publishing its decoded state before
    /// every interpreted instruction, because the debugger may resume into any
    /// of them. That is the right cost while someone is stepping and the wrong
    /// one when nobody is: a recursive program asked only to run to its end
    /// would spend minutes encoding state no one reads. Writing the switch is
    /// how the session says which of the two it is.
    pub fn set_vm_stops(&mut self, wanted: bool) -> Result<(), String> {
        let Some(probe) = self.target.probe.clone() else {
            return Ok(());
        };
        if self.client.state().stop().is_none() {
            return Ok(());
        }
        let frame_id = self.top_frame_id()?;
        let value = u32::from(wanted);
        // Written through the command interpreter rather than the protocol's
        // own `writeMemory`: the LLDB the Swift toolchains ship exits on that
        // request, taking the session with it, and a debugger that dies while
        // tidying up is worse than one that is slow.
        self.client
            .request(
                "evaluate",
                json!({
                    "expression": format!(
                        "memory write -s 4 &{} {value}",
                        probe.active_symbol
                    ),
                    "frameId": frame_id,
                    "context": "repl",
                }),
                DEFAULT_TIMEOUT,
            )
            // A target built before the switch existed has no such symbol.
            // Reporting that would turn a slow `finish` into a failed one,
            // and it is still correct there, only slower.
            .ok();
        Ok(())
    }

    /// The identifier of the innermost frame of the stopped thread.
    pub fn top_frame_id(&mut self) -> Result<i64, String> {
        let thread_id = self
            .client
            .stopped_thread()
            .map_err(|error| error.to_string())?;
        let stack = self
            .client
            .request(
                "stackTrace",
                json!({ "threadId": thread_id, "startFrame": 0, "levels": 1 }),
                DEFAULT_TIMEOUT,
            )
            .map_err(|error| error.to_string())?;
        stack["stackFrames"][0]["id"]
            .as_i64()
            .ok_or_else(|| "the stopped thread reported no frame".to_owned())
    }

    /// Ends the session and removes the artifacts its target owned.
    pub fn close(self) -> (Option<i32>, String) {
        self.target.clean();
        self.client.disconnect(true)
    }
}

/// Copies one adapter resolution onto the breakpoint that asked for it.
fn apply_resolution(breakpoint: &mut Breakpoint, resolved: Option<&Value>) {
    let Some(resolved) = resolved else {
        breakpoint.verified = false;
        breakpoint.address = None;
        return;
    };
    breakpoint.verified = resolved["verified"].as_bool().unwrap_or(false);
    breakpoint.address = resolved["instructionReference"].as_str().map(str::to_owned);
}

/// The probe breakpoint to install, if any.
///
/// The probe is a breakpoint on every interpreted instruction, so it exists
/// only when something wants those stops.
///
/// While stepping it carries no condition: a step resumes to the *next*
/// instruction, and the condition that put the session where it is names the
/// one location it already reached. With Kira breakpoints it carries theirs.
/// With neither it is left out entirely — an unconditional probe would stop a
/// program millions of times on its way to an end the caller asked it to
/// simply run to.
fn probe_request(stepping: bool, kira: &[&Breakpoint], target: &PreparedTarget) -> Option<Value> {
    let probe = target.probe.as_ref()?;
    match (stepping, probe_condition(kira, target)) {
        (true, _) => Some(json!({ "name": probe.symbol })),
        (false, Some(condition)) => Some(json!({ "name": probe.symbol, "condition": condition })),
        (false, None) => None,
    }
}

/// The probe condition that stops at any of the requested Kira locations.
///
/// `None` means the probe carries no condition — either because nothing was
/// requested, or because this host's calling convention has no known probe
/// registers. The caller decides what that means: during a step the probe runs
/// unconditionally, and outside one it is not installed at all.
fn probe_condition(kira: &[&Breakpoint], target: &PreparedTarget) -> Option<String> {
    let probe = target.probe.as_ref()?;
    if kira.is_empty() {
        return None;
    }
    let mut conditions = Vec::with_capacity(kira.len());
    for breakpoint in kira {
        let Placement::Kira {
            function_id, pc, ..
        } = &breakpoint.placement
        else {
            continue;
        };
        let condition = probe.condition(*function_id, *pc)?;
        conditions.push(match &breakpoint.condition {
            Some(extra) => format!("({condition} && ({extra}))"),
            None => condition,
        });
    }
    match conditions.is_empty() {
        true => None,
        false => Some(conditions.join(" || ")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kira_debug::{Backend, DebugFunction, DebugInfo, DebugSource};

    fn target(backend: Backend) -> PreparedTarget {
        let info = DebugInfo {
            module_name: "buggy".to_owned(),
            backend,
            source: Some(DebugSource {
                path: PathBuf::from("buggy.kira"),
            }),
            functions: vec![DebugFunction {
                id: 4,
                name: "discountAmount".to_owned(),
                backend,
                symbol: matches!(backend, Backend::Llvm)
                    .then(|| "kira_fn_4_discountAmount".to_owned()),
                line: 75,
            }],
            optimized: false,
        };
        PreparedTarget::new(&info, "kira.exe")
    }

    fn kira_breakpoint(id: u32, pc: u32, condition: Option<&str>) -> Breakpoint {
        let placement = Placement::Kira {
            function: "discountAmount".to_owned(),
            function_id: 4,
            pc,
        };
        Breakpoint {
            id,
            location: placement.location(),
            placement,
            condition: condition.map(str::to_owned),
            verified: false,
            address: None,
        }
    }

    #[test]
    fn several_kira_breakpoints_become_one_probe_condition() {
        let target = target(Backend::Vm);
        let first = kira_breakpoint(1, 0, None);
        let second = kira_breakpoint(2, 3, None);
        assert_eq!(
            probe_condition(&[&first, &second], &target).as_deref(),
            Some("($rcx == 4 && $rdx == 0) || ($rcx == 4 && $rdx == 3)")
        );
    }

    /// A caller's own condition narrows the location rather than replacing it.
    #[test]
    fn a_caller_condition_is_combined_with_the_location_it_applies_to() {
        let target = target(Backend::Vm);
        let breakpoint = kira_breakpoint(1, 0, Some("$rax > 100"));
        assert_eq!(
            probe_condition(&[&breakpoint], &target).as_deref(),
            Some("(($rcx == 4 && $rdx == 0) && ($rax > 100))")
        );
    }

    #[test]
    fn no_kira_breakpoints_means_no_condition_to_install() {
        let target = target(Backend::Vm);
        assert!(probe_condition(&[], &target).is_none());
    }

    /// A program with nothing requested of the VM must run at full speed: an
    /// unconditional probe would stop it once per interpreted instruction, and
    /// a program asked only to finish would never get there.
    #[test]
    fn a_session_with_no_kira_breakpoints_installs_no_probe() {
        let target = target(Backend::Vm);
        assert_eq!(probe_request(false, &[], &target), None);
    }

    #[test]
    fn a_step_installs_the_probe_without_a_condition() {
        let target = target(Backend::Vm);
        assert_eq!(
            probe_request(true, &[], &target),
            Some(json!({ "name": "kira_vm_debug_probe" }))
        );
    }

    #[test]
    fn a_kira_breakpoint_installs_the_probe_with_its_condition() {
        let target = target(Backend::Vm);
        let breakpoint = kira_breakpoint(1, 2, None);
        assert_eq!(
            probe_request(false, &[&breakpoint], &target),
            Some(json!({
                "name": "kira_vm_debug_probe",
                "condition": "($rcx == 4 && $rdx == 2)",
            }))
        );
    }

    #[test]
    fn a_native_target_never_installs_a_probe() {
        let target = target(Backend::Llvm);
        assert_eq!(probe_request(true, &[], &target), None);
    }

    #[test]
    fn a_native_target_has_no_probe_to_condition() {
        let target = target(Backend::Llvm);
        let breakpoint = kira_breakpoint(1, 0, None);
        assert!(probe_condition(&[&breakpoint], &target).is_none());
    }

    #[test]
    fn a_placement_reports_the_location_a_caller_wrote() {
        assert_eq!(
            Placement::Kira {
                function: "discountAmount".to_owned(),
                function_id: 4,
                pc: 3,
            }
            .location(),
            "discountAmount:3"
        );
        assert_eq!(
            Placement::Function {
                symbol: "kira_fn_0_main".to_owned()
            }
            .location(),
            "kira_fn_0_main"
        );
        assert_eq!(
            Placement::Source {
                path: PathBuf::from("buggy.kira"),
                line: 75,
            }
            .location(),
            "buggy.kira:75"
        );
    }

    #[test]
    fn an_unresolved_breakpoint_reports_no_address() {
        let mut breakpoint = kira_breakpoint(1, 0, None);
        apply_resolution(
            &mut breakpoint,
            Some(&json!({ "verified": true, "instructionReference": "0x1000" })),
        );
        assert!(breakpoint.verified);
        assert_eq!(breakpoint.address.as_deref(), Some("0x1000"));
        apply_resolution(&mut breakpoint, None);
        assert!(!breakpoint.verified);
        assert!(breakpoint.address.is_none());
    }
}
