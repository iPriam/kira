//! LLDB's Debug Adapter Protocol transport.
//!
//! The command-line LLDB frontend shipped with some Windows toolchains aborts
//! while resuming a second stop. `lldb-dap` uses the same LLDB engine without
//! that command-interpreter path, so Kira can still run a real multi-stop
//! session and read the VM's exported state through the standard debugger
//! protocol.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command, Stdio};

use serde_json::{Value, json};

use crate::lldb::{LldbError, LldbOutput, configure_lldb_environment};

const TEXT_READ_LIMIT: usize = 4096;

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
        let executable = std::env::var_os("KIRA_LLDB_DAP")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("lldb-dap"));
        let mut command = Command::new(&executable);
        configure_lldb_environment(&mut command);
        command
            .arg("--pre-init-command")
            .arg("settings set target.inline-breakpoint-strategy always")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let child = command
            .spawn()
            .map_err(|source| LldbError::Spawn { executable, source })?;
        let mut session = match DapSession::new(child) {
            Ok(session) => session,
            Err((mut child, message)) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(protocol_error(message, None, String::new()));
            }
        };
        let protocol_result = session.run(self);
        if let Err(message) = &protocol_result {
            session.stop_target();
            return session.finish(Some(message.clone()));
        }
        session.finish(None)
    }
}

struct DapSession {
    child: Child,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
    error: ChildStderr,
    next_sequence: u64,
    transcript: String,
}

impl DapSession {
    fn new(mut child: Child) -> Result<Self, (Child, String)> {
        let Some(input) = child.stdin.take() else {
            return Err((child, "lldb-dap did not expose stdin".to_owned()));
        };
        let Some(output) = child.stdout.take() else {
            return Err((child, "lldb-dap did not expose stdout".to_owned()));
        };
        let Some(error) = child.stderr.take() else {
            return Err((child, "lldb-dap did not expose stderr".to_owned()));
        };
        Ok(Self {
            child,
            input,
            output: BufReader::new(output),
            error,
            next_sequence: 1,
            transcript: String::new(),
        })
    }

    fn run(&mut self, launch: &LldbDapLaunch) -> Result<(), String> {
        let initialize = self.send(
            "initialize",
            json!({
                "clientID": "kira",
                "clientName": "kira",
                "adapterID": "lldb-dap",
                "pathFormat": "path",
                "linesStartAt1": true,
                "columnsStartAt1": true,
            }),
        )?;
        self.wait_response(initialize, "initialize")?;

        let launch_sequence = self.send(
            "launch",
            json!({
                "program": launch.target,
                "args": launch.arguments,
                "cwd": std::env::current_dir()
                    .map_err(|error| error.to_string())?,
                "stopAtEntry": false,
                "noDebug": false,
            }),
        )?;
        self.wait_for_launch(launch_sequence)?;

        let breakpoints = launch
            .breakpoints
            .iter()
            .map(|breakpoint| {
                let mut value = serde_json::Map::new();
                value.insert("name".to_owned(), Value::String(breakpoint.name.clone()));
                if let Some(condition) = &breakpoint.condition {
                    value.insert("condition".to_owned(), Value::String(condition.clone()));
                }
                Value::Object(value)
            })
            .collect::<Vec<_>>();
        let breakpoint_sequence = self.send(
            "setFunctionBreakpoints",
            json!({ "breakpoints": breakpoints }),
        )?;
        let breakpoint_response =
            self.wait_response(breakpoint_sequence, "setFunctionBreakpoints")?;
        self.append_breakpoint_report(&breakpoint_response);

        let configuration = self.send("configurationDone", json!({}))?;
        self.wait_response(configuration, "configurationDone")?;

        let mut stopped = self.wait_for_stop()?;
        for stop_index in 0..=launch.continue_count {
            self.inspect_stop(&stopped, stop_index + 1, launch)?;
            if stop_index == launch.continue_count {
                break;
            }
            let sequence = self.send("continue", json!({ "threadId": stopped.thread_id }))?;
            self.wait_response(sequence, "continue")?;
            stopped = self.wait_for_stop()?;
        }

        let disconnect = self.send("disconnect", json!({ "terminateDebuggee": true }))?;
        self.wait_response(disconnect, "disconnect")?;
        Ok(())
    }

    fn send(&mut self, command: &str, arguments: Value) -> Result<u64, String> {
        let sequence = self.next_sequence;
        self.next_sequence += 1;
        let message = json!({
            "seq": sequence,
            "type": "request",
            "command": command,
            "arguments": arguments,
        });
        let body = serde_json::to_vec(&message).map_err(|error| error.to_string())?;
        write!(self.input, "Content-Length: {}\r\n\r\n", body.len())
            .map_err(|error| error.to_string())?;
        self.input
            .write_all(&body)
            .map_err(|error| error.to_string())?;
        self.input.flush().map_err(|error| error.to_string())?;
        Ok(sequence)
    }

    fn read_message(&mut self) -> Result<Value, String> {
        let mut content_length = None;
        loop {
            let mut line = String::new();
            let read = self
                .output
                .read_line(&mut line)
                .map_err(|error| error.to_string())?;
            if read == 0 {
                return Err("lldb-dap closed before sending a message".to_owned());
            }
            if line == "\r\n" {
                break;
            }
            if let Some(value) = line.strip_prefix("Content-Length:") {
                content_length = Some(
                    value
                        .trim()
                        .parse::<usize>()
                        .map_err(|error| error.to_string())?,
                );
            }
        }
        let length =
            content_length.ok_or_else(|| "DAP message has no content length".to_owned())?;
        let mut body = vec![0; length];
        self.output
            .read_exact(&mut body)
            .map_err(|error| error.to_string())?;
        serde_json::from_slice(&body).map_err(|error| error.to_string())
    }

    fn wait_response(&mut self, sequence: u64, command: &str) -> Result<Value, String> {
        loop {
            let message = self.read_message()?;
            self.record_event(&message);
            if message.get("type").and_then(Value::as_str) != Some("response")
                || message.get("request_seq").and_then(Value::as_u64) != Some(sequence)
            {
                continue;
            }
            if message.get("success").and_then(Value::as_bool) != Some(true) {
                return Err(format!(
                    "LLDB DAP `{command}` failed: {}",
                    message
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("unknown error")
                ));
            }
            return Ok(message);
        }
    }

    fn wait_for_launch(&mut self, sequence: u64) -> Result<(), String> {
        let mut response_seen = false;
        let mut initialized_seen = false;
        while !response_seen || !initialized_seen {
            let message = self.read_message()?;
            self.record_event(&message);
            if message.get("type").and_then(Value::as_str) == Some("event")
                && message.get("event").and_then(Value::as_str) == Some("initialized")
            {
                initialized_seen = true;
            }
            if message.get("type").and_then(Value::as_str) == Some("response")
                && message.get("request_seq").and_then(Value::as_u64) == Some(sequence)
            {
                if message.get("success").and_then(Value::as_bool) != Some(true) {
                    return Err(format!(
                        "LLDB DAP launch failed: {}",
                        message
                            .get("message")
                            .and_then(Value::as_str)
                            .unwrap_or("unknown error")
                    ));
                }
                response_seen = true;
            }
        }
        Ok(())
    }

    fn wait_for_stop(&mut self) -> Result<StoppedThread, String> {
        loop {
            let message = self.read_message()?;
            self.record_event(&message);
            if message.get("type").and_then(Value::as_str) != Some("event") {
                continue;
            }
            match message.get("event").and_then(Value::as_str) {
                Some("stopped") => {
                    let thread_id = message
                        .get("body")
                        .and_then(|body| body.get("threadId"))
                        .and_then(Value::as_u64)
                        .ok_or_else(|| "LLDB DAP stop has no thread id".to_owned())?;
                    return Ok(StoppedThread { thread_id });
                }
                Some("exited") | Some("terminated") => {
                    return Err("LLDB target exited before the requested stop".to_owned());
                }
                _ => {}
            }
        }
    }

    fn inspect_stop(
        &mut self,
        stopped: &StoppedThread,
        index: usize,
        launch: &LldbDapLaunch,
    ) -> Result<(), String> {
        let stack_sequence = self.send(
            "stackTrace",
            json!({ "threadId": stopped.thread_id, "levels": 1 }),
        )?;
        let stack = self.wait_response(stack_sequence, "stackTrace")?;
        let frame = stack
            .get("body")
            .and_then(|body| body.get("stackFrames"))
            .and_then(Value::as_array)
            .and_then(|frames| frames.first())
            .ok_or_else(|| "LLDB DAP stop has no stack frame".to_owned())?;
        let frame_id = frame
            .get("id")
            .and_then(Value::as_u64)
            .ok_or_else(|| "LLDB DAP frame has no id".to_owned())?;
        let frame_name = frame
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("<unknown>");
        self.transcript
            .push_str(&format!("lldb-dap-stop #{index} frame={frame_name}\n"));

        if let Some(symbol) = &launch.text_symbol {
            let evaluate_sequence = self.send(
                "evaluate",
                json!({
                    "expression": format!("&{symbol}"),
                    "frameId": frame_id,
                    "context": "repl",
                }),
            )?;
            let evaluation = self.wait_response(evaluate_sequence, "evaluate")?;
            let address = parse_address(
                evaluation
                    .get("body")
                    .and_then(|body| body.get("result"))
                    .and_then(Value::as_str)
                    .ok_or_else(|| "LLDB DAP symbol evaluation has no result".to_owned())?,
            )?;
            let read_sequence = self.send(
                "readMemory",
                json!({
                    "memoryReference": address,
                    "offset": 0,
                    "count": TEXT_READ_LIMIT,
                }),
            )?;
            let memory = self.wait_response(read_sequence, "readMemory")?;
            let text = memory
                .get("body")
                .and_then(|body| body.get("data"))
                .and_then(Value::as_str)
                .ok_or_else(|| "LLDB DAP text read has no data".to_owned())?;
            let bytes = decode_base64(text)?;
            let length = bytes
                .iter()
                .position(|byte| *byte == 0)
                .unwrap_or(bytes.len());
            self.transcript
                .push_str(&String::from_utf8_lossy(&bytes[..length]));
        }
        if launch.disassemble {
            self.append_disassembly(frame, frame_id)?;
        }
        Ok(())
    }

    fn append_disassembly(&mut self, frame: &Value, frame_id: u64) -> Result<(), String> {
        let Some(reference) = frame
            .get("instructionPointerReference")
            .and_then(Value::as_str)
        else {
            return Ok(());
        };
        let sequence = self.send(
            "disassemble",
            json!({
                "memoryReference": reference,
                "instructionOffset": 0,
                "instructionCount": 32,
                "resolveSymbols": true,
                "frameId": frame_id,
            }),
        )?;
        let response = self.wait_response(sequence, "disassemble")?;
        let Some(instructions) = response
            .get("body")
            .and_then(|body| body.get("instructions"))
            .and_then(Value::as_array)
        else {
            return Ok(());
        };
        self.transcript.push_str("cpu-instructions:\n");
        for instruction in instructions {
            let address = instruction
                .get("address")
                .and_then(Value::as_str)
                .unwrap_or("<address>");
            let text = instruction
                .get("instruction")
                .and_then(Value::as_str)
                .unwrap_or("<instruction>");
            self.transcript.push_str(&format!("  {address}: {text}\n"));
        }
        Ok(())
    }

    fn append_breakpoint_report(&mut self, response: &Value) {
        let Some(breakpoints) = response
            .get("body")
            .and_then(|body| body.get("breakpoints"))
            .and_then(Value::as_array)
        else {
            return;
        };
        for breakpoint in breakpoints {
            let verified = breakpoint
                .get("verified")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let reference = breakpoint
                .get("instructionReference")
                .and_then(Value::as_str)
                .unwrap_or("<unresolved>");
            self.transcript.push_str(&format!(
                "lldb-dap-breakpoint verified={verified} address={reference}\n"
            ));
        }
    }

    fn record_event(&mut self, message: &Value) {
        if message.get("type").and_then(Value::as_str) != Some("event") {
            return;
        }
        if message.get("event").and_then(Value::as_str) != Some("output") {
            return;
        }
        if let Some(output) = message
            .get("body")
            .and_then(|body| body.get("output"))
            .and_then(Value::as_str)
        {
            self.transcript.push_str(output);
        }
    }

    fn stop_target(&mut self) {
        let _ = self.child.kill();
    }

    fn finish(mut self, protocol_message: Option<String>) -> Result<LldbOutput, LldbError> {
        drop(self.input);
        let status = self.child.wait();
        let mut stderr = String::new();
        let _ = self.error.read_to_string(&mut stderr);
        let code = status.as_ref().ok().and_then(|status| status.code());
        if let Some(message) = protocol_message {
            return Err(protocol_error(message, code, stderr));
        }
        if !status.is_ok_and(|status| status.success()) {
            return Err(LldbError::Failed {
                code,
                stdout: self.transcript,
                stderr,
            });
        }
        Ok(LldbOutput {
            stdout: self.transcript,
            stderr,
        })
    }
}

#[derive(Debug, Clone, Copy)]
struct StoppedThread {
    thread_id: u64,
}

fn parse_address(value: &str) -> Result<String, String> {
    let start = value
        .find("0x")
        .ok_or_else(|| format!("LLDB DAP expression is not an address: {value}"))?;
    let address = value[start..]
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .trim_end_matches(',')
        .to_owned();
    if address.len() <= 2 {
        return Err(format!("LLDB DAP expression is not an address: {value}"));
    }
    Ok(address)
}

fn decode_base64(value: &str) -> Result<Vec<u8>, String> {
    let alphabet = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut output = Vec::with_capacity(value.len() * 3 / 4);
    let mut quartet = [0_u8; 4];
    let mut count = 0;
    for byte in value.bytes() {
        if byte == b'=' {
            break;
        }
        let Some(index) = alphabet.iter().position(|item| *item == byte) else {
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
    Ok(output)
}

fn protocol_error(message: String, code: Option<i32>, stderr: String) -> LldbError {
    LldbError::Failed {
        code,
        stdout: String::new(),
        stderr: if stderr.is_empty() {
            message
        } else {
            format!("{message}\n{stderr}")
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
            Ok("0x00007ff708a28f60".to_owned())
        );
    }

    #[test]
    fn decodes_dap_memory_base64_without_an_external_codec() {
        assert_eq!(decode_base64("S2lyYQ==").unwrap(), b"Kira");
    }

    #[test]
    fn breakpoints_keep_conditions_separate_from_names() {
        let breakpoint = LldbDapBreakpoint::new("probe").with_condition("$rcx == 5");
        assert_eq!(breakpoint.name, "probe");
        assert_eq!(breakpoint.condition.as_deref(), Some("$rcx == 5"));
    }
}
