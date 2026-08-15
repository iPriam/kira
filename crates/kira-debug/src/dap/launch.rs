//! The scripted DAP launch behind `kira debug --lldb-dap`.
//!
//! One run with a fixed shape: launch, break on the VM probe, and report each
//! stop the caller asked for as a transcript. The interactive session in
//! [`super::client`] is the same connection driven by a caller that decides
//! what to do next; this is the batch form the command line prints.

use std::path::PathBuf;
use std::time::Duration;

use serde_json::{Value, json};

use super::client::{DEFAULT_TIMEOUT, DapClient, DapError};
use crate::engine::Engine;
use crate::lldb::{LldbError, LldbOutput};

/// How much of a null-terminated state string is read from the target.
const TEXT_READ_LIMIT: usize = 4096;
/// How long a launched target may take to reach its first stop.
const LAUNCH_TIMEOUT: Duration = Duration::from_secs(120);

/// A function breakpoint sent to LLDB DAP.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LldbDapBreakpoint {
    /// The native symbol or function name.
    pub name: String,
    /// Optional LLDB expression evaluated at the native stop.
    pub condition: Option<String>,
}

impl LldbDapBreakpoint {
    /// Creates an unconditional function breakpoint.
    #[must_use]
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            condition: None,
        }
    }

    /// Adds an LLDB condition to this breakpoint.
    #[must_use]
    pub fn with_condition(mut self, condition: impl Into<String>) -> Self {
        self.condition = Some(condition.into());
        self
    }

    /// This breakpoint as a DAP `setFunctionBreakpoints` entry.
    #[must_use]
    pub fn to_request(&self) -> Value {
        match &self.condition {
            Some(condition) => json!({ "name": self.name, "condition": condition }),
            None => json!({ "name": self.name }),
        }
    }
}

/// A real LLDB-DAP launch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LldbDapLaunch {
    /// The executable LLDB should debug.
    pub target: PathBuf,
    /// Arguments passed to the target process.
    pub arguments: Vec<String>,
    /// Native function breakpoints.
    pub breakpoints: Vec<LldbDapBreakpoint>,
    /// Exported C symbol containing a null-terminated stop description.
    pub text_symbol: Option<String>,
    /// Whether to request CPU instructions at each stopped frame.
    pub disassemble: bool,
    /// Number of additional `continue` requests after the first stop.
    pub continue_count: usize,
}

impl LldbDapLaunch {
    /// Creates a DAP launch for an executable.
    #[must_use]
    pub fn new(target: impl Into<PathBuf>) -> Self {
        Self {
            target: target.into(),
            arguments: Vec::new(),
            breakpoints: Vec::new(),
            text_symbol: None,
            disassemble: false,
            continue_count: 0,
        }
    }

    /// Adds a function breakpoint.
    pub fn add_breakpoint(&mut self, breakpoint: LldbDapBreakpoint) {
        if !self.breakpoints.contains(&breakpoint) {
            self.breakpoints.push(breakpoint);
        }
    }

    /// Requests readable text from an exported stopped-state symbol.
    pub fn set_text_symbol(&mut self, symbol: impl Into<String>) {
        self.text_symbol = Some(symbol.into());
    }

    /// Enables or disables CPU instruction inspection.
    pub fn set_disassemble(&mut self, enabled: bool) {
        self.disassemble = enabled;
    }

    /// Sets how many later VM probe stops should be resumed into.
    pub fn set_continue_count(&mut self, count: usize) {
        self.continue_count = count;
    }

    /// Launches `lldb-dap` and drives a real stopped-process session.
    pub fn launch(&self) -> Result<LldbOutput, LldbError> {
        let mut client = DapClient::start(Engine::DebugAdapter).map_err(dap_error)?;
        let mut transcript = String::new();
        let result = self.run(&mut client, &mut transcript);
        let target_output = client.take_output();
        let (code, stderr) = client.disconnect(true);
        transcript.push_str(&target_output);
        match result {
            Ok(()) => Ok(LldbOutput {
                stdout: transcript,
                stderr,
            }),
            Err(error) => Err(LldbError::Failed {
                code,
                stdout: transcript,
                stderr: match stderr.is_empty() {
                    true => error.to_string(),
                    false => format!("{error}\n{stderr}"),
                },
            }),
        }
    }

    fn run(&self, client: &mut DapClient, transcript: &mut String) -> Result<(), DapError> {
        let capabilities = client.request(
            "initialize",
            json!({
                "clientID": "kira",
                "clientName": "kira",
                "adapterID": "lldb-dap",
                "pathFormat": "path",
                "linesStartAt1": true,
                "columnsStartAt1": true,
            }),
            DEFAULT_TIMEOUT,
        )?;
        client.set_capabilities(capabilities);

        client.await_configuration(
            "launch",
            json!({
                "program": self.target,
                "args": self.arguments,
                "cwd": std::env::current_dir().unwrap_or_default(),
                "stopAtEntry": false,
                "noDebug": false,
            }),
            LAUNCH_TIMEOUT,
        )?;

        let breakpoints = self
            .breakpoints
            .iter()
            .map(LldbDapBreakpoint::to_request)
            .collect::<Vec<_>>();
        let reply = client.request(
            "setFunctionBreakpoints",
            json!({ "breakpoints": breakpoints }),
            DEFAULT_TIMEOUT,
        )?;
        transcript.push_str(&breakpoint_report(&reply));

        client.request("configurationDone", json!({}), DEFAULT_TIMEOUT)?;

        for index in 0..=self.continue_count {
            let stop = client.wait_for_stop(LAUNCH_TIMEOUT)?;
            self.inspect(client, transcript, index + 1)?;
            if index == self.continue_count {
                break;
            }
            client.request(
                "continue",
                json!({ "threadId": stop.thread_id }),
                DEFAULT_TIMEOUT,
            )?;
            client.mark_running();
        }
        Ok(())
    }

    fn inspect(
        &self,
        client: &mut DapClient,
        transcript: &mut String,
        index: usize,
    ) -> Result<(), DapError> {
        let thread_id = client.stopped_thread()?;
        let stack = client.request(
            "stackTrace",
            json!({ "threadId": thread_id, "levels": 1 }),
            DEFAULT_TIMEOUT,
        )?;
        let frame = stack["stackFrames"]
            .as_array()
            .and_then(|frames| frames.first())
            .cloned()
            .unwrap_or(Value::Null);
        let frame_id = frame["id"].as_i64().unwrap_or_default();
        let frame_name = frame["name"].as_str().unwrap_or("<unknown>");
        transcript.push_str(&format!("lldb-dap-stop #{index} frame={frame_name}\n"));

        if let Some(symbol) = &self.text_symbol {
            transcript.push_str(&read_text_symbol(client, symbol, frame_id)?);
        }
        if self.disassemble
            && let Some(reference) = frame["instructionPointerReference"].as_str()
        {
            let instructions = client.request(
                "disassemble",
                json!({
                    "memoryReference": reference,
                    "instructionOffset": 0,
                    "instructionCount": 32,
                    "resolveSymbols": true,
                    "frameId": frame_id,
                }),
                DEFAULT_TIMEOUT,
            )?;
            transcript.push_str(&disassembly_report(&instructions));
        }
        Ok(())
    }
}

/// Reads a null-terminated string the target exports under `symbol`.
fn read_text_symbol(
    client: &mut DapClient,
    symbol: &str,
    frame_id: i64,
) -> Result<String, DapError> {
    let evaluation = client.request(
        "evaluate",
        json!({
            "expression": format!("&{symbol}"),
            "frameId": frame_id,
            "context": "repl",
        }),
        DEFAULT_TIMEOUT,
    )?;
    let Some(address) = evaluation["result"].as_str().and_then(parse_address) else {
        return Ok(String::new());
    };
    let memory = client.request(
        "readMemory",
        json!({ "memoryReference": address, "offset": 0, "count": TEXT_READ_LIMIT }),
        DEFAULT_TIMEOUT,
    )?;
    let Some(data) = memory["data"].as_str() else {
        return Ok(String::new());
    };
    let bytes = decode_base64(data);
    let length = bytes
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(bytes.len());
    Ok(String::from_utf8_lossy(&bytes[..length]).into_owned())
}

/// The transcript lines describing where each breakpoint resolved.
fn breakpoint_report(reply: &Value) -> String {
    let Some(breakpoints) = reply["breakpoints"].as_array() else {
        return String::new();
    };
    breakpoints
        .iter()
        .map(|breakpoint| {
            let verified = breakpoint["verified"].as_bool().unwrap_or(false);
            let address = breakpoint["instructionReference"]
                .as_str()
                .unwrap_or("<unresolved>");
            format!("lldb-dap-breakpoint verified={verified} address={address}\n")
        })
        .collect()
}

/// The transcript lines describing a stopped frame's CPU instructions.
fn disassembly_report(reply: &Value) -> String {
    let Some(instructions) = reply["instructions"].as_array() else {
        return String::new();
    };
    let mut report = String::from("cpu-instructions:\n");
    for instruction in instructions {
        let address = instruction["address"].as_str().unwrap_or("<address>");
        let text = instruction["instruction"]
            .as_str()
            .unwrap_or("<instruction>");
        report.push_str(&format!("  {address}: {text}\n"));
    }
    report
}

/// The address an LLDB expression result names.
#[must_use]
pub fn parse_address(value: &str) -> Option<String> {
    let start = value.find("0x")?;
    let address = value[start..]
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .trim_end_matches(',')
        .to_owned();
    (address.len() > 2).then_some(address)
}

/// Decodes the base64 DAP carries memory in.
///
/// The alphabet is fixed and four characters wide; a dependency for this would
/// be a dependency the whole toolchain then carries.
#[must_use]
pub fn decode_base64(value: &str) -> Vec<u8> {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut output = Vec::with_capacity(value.len() * 3 / 4);
    let mut quartet = [0_u8; 4];
    let mut count = 0;
    for byte in value.bytes() {
        if byte == b'=' {
            break;
        }
        let Some(index) = ALPHABET.iter().position(|item| *item == byte) else {
            continue;
        };
        quartet[count] = index as u8;
        count += 1;
        if count == 4 {
            output.push((quartet[0] << 2) | (quartet[1] >> 4));
            output.push((quartet[1] << 4) | (quartet[2] >> 2));
            output.push((quartet[2] << 6) | quartet[3]);
            count = 0;
        }
    }
    if count >= 2 {
        output.push((quartet[0] << 2) | (quartet[1] >> 4));
    }
    if count >= 3 {
        output.push((quartet[1] << 4) | (quartet[2] >> 2));
    }
    output
}

/// Reports a session failure in the shape every LLDB caller already handles.
fn dap_error(error: DapError) -> LldbError {
    match error {
        DapError::Spawn { executable, source } => LldbError::Spawn { executable, source },
        other => LldbError::Failed {
            code: None,
            stdout: String::new(),
            stderr: other.to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_lldb_expression_addresses() {
        assert_eq!(
            parse_address("(void **) $0 = 0x00007ff708a28f60"),
            Some("0x00007ff708a28f60".to_owned())
        );
        assert_eq!(parse_address("no address here"), None);
    }

    #[test]
    fn decodes_dap_memory_base64_without_an_external_codec() {
        assert_eq!(decode_base64("S2lyYQ=="), b"Kira");
    }

    #[test]
    fn breakpoints_keep_conditions_separate_from_names() {
        let breakpoint = LldbDapBreakpoint::new("probe").with_condition("$rcx == 5");
        assert_eq!(breakpoint.name, "probe");
        assert_eq!(breakpoint.condition.as_deref(), Some("$rcx == 5"));
        assert_eq!(
            breakpoint.to_request(),
            json!({ "name": "probe", "condition": "$rcx == 5" })
        );
    }

    #[test]
    fn the_breakpoint_report_names_every_resolution() {
        let reply = json!({
            "breakpoints": [
                { "verified": true, "instructionReference": "0x1000" },
                { "verified": false },
            ]
        });
        assert_eq!(
            breakpoint_report(&reply),
            "lldb-dap-breakpoint verified=true address=0x1000\n\
             lldb-dap-breakpoint verified=false address=<unresolved>\n"
        );
    }

    #[test]
    fn the_disassembly_report_lists_each_instruction() {
        let reply = json!({
            "instructions": [{ "address": "0x1000", "instruction": "pushq %rbp" }]
        });
        assert_eq!(
            disassembly_report(&reply),
            "cpu-instructions:\n  0x1000: pushq %rbp\n"
        );
    }
}
