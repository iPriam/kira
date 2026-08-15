//! An interactive Debug Adapter Protocol client.
//!
//! One long-lived debug session: the target is launched once and then stepped,
//! resumed, and inspected by whatever drives this client. Requests and the
//! adapter's asynchronous events share one connection, so every request pumps
//! events until its own reply arrives and the session's view of the target —
//! stopped, running, exited — stays current without a second reader.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use serde_json::{Value, json};

use super::transport::{Transport, TransportError};
use crate::engine::{self, Engine};

/// How long a request waits for its reply before the caller is told.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

/// What the debugged process is doing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TargetState {
    /// Launched and configured, but not yet resumed.
    Configuring,
    /// Executing.
    Running,
    /// Stopped, with the reason the adapter reported.
    Stopped(Stop),
    /// Exited with a status code.
    Exited(i32),
    /// Gone without reporting a status code.
    Terminated,
}

impl TargetState {
    /// The stop this state describes, when it is a stop.
    #[must_use]
    pub const fn stop(&self) -> Option<&Stop> {
        match self {
            Self::Stopped(stop) => Some(stop),
            _ => None,
        }
    }

    /// Whether the target can still be resumed or inspected.
    #[must_use]
    pub const fn is_alive(&self) -> bool {
        matches!(self, Self::Configuring | Self::Running | Self::Stopped(_))
    }

    /// The word a report uses for this state.
    #[must_use]
    pub const fn label(&self) -> &'static str {
        match self {
            Self::Configuring => "configuring",
            Self::Running => "running",
            Self::Stopped(_) => "stopped",
            Self::Exited(_) => "exited",
            Self::Terminated => "terminated",
        }
    }
}

/// One reported stop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stop {
    /// The thread the adapter stopped.
    pub thread_id: i64,
    /// The adapter's stop reason, such as `breakpoint` or `step`.
    pub reason: String,
    /// The adapter's human-readable detail, when it sent one.
    pub description: Option<String>,
    /// The breakpoint identifiers this stop hit.
    pub hit_breakpoints: Vec<i64>,
}

/// Why a debug session request did not succeed.
#[derive(Debug, thiserror::Error)]
pub enum DapError {
    /// The adapter executable could not be started.
    #[error("cannot start the debug adapter `{executable}`: {source}")]
    Spawn {
        /// The executable that was tried.
        executable: PathBuf,
        /// The process error.
        #[source]
        source: std::io::Error,
    },
    /// The connection to the adapter failed.
    #[error("{0}")]
    Transport(TransportError),
    /// The adapter refused a request.
    #[error("`{command}` failed: {message}")]
    Request {
        /// The DAP request that was refused.
        command: String,
        /// The adapter's message.
        message: String,
    },
    /// The request needs a stopped target and the target is not stopped.
    #[error("the target is {state} rather than stopped")]
    NotStopped {
        /// The state the target was in.
        state: &'static str,
    },
}

impl From<TransportError> for DapError {
    fn from(error: TransportError) -> Self {
        Self::Transport(error)
    }
}

/// A live debug adapter session.
pub struct DapClient {
    transport: Transport,
    state: TargetState,
    output: String,
    capabilities: Value,
    executable: PathBuf,
}

impl DapClient {
    /// Starts the adapter `engine` names and connects to it.
    pub fn start(engine: Engine) -> Result<Self, DapError> {
        let executable = engine.executable();
        let mut command = Command::new(&executable);
        engine::configure(&mut command, &executable);
        command
            .arg("--pre-init-command")
            .arg("settings set target.inline-breakpoint-strategy always")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let child = command.spawn().map_err(|source| DapError::Spawn {
            executable: executable.clone(),
            source,
        })?;
        let transport = match Transport::new(child) {
            Ok(transport) => transport,
            Err((mut child, message)) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(DapError::Transport(TransportError::Protocol(message)));
            }
        };
        Ok(Self {
            transport,
            state: TargetState::Configuring,
            output: String::new(),
            capabilities: Value::Null,
            executable,
        })
    }

    /// The adapter executable this session is talking to.
    #[must_use]
    pub fn executable(&self) -> &Path {
        &self.executable
    }

    /// What the target is doing, as of the last message read.
    #[must_use]
    pub fn state(&self) -> &TargetState {
        &self.state
    }

    /// The capabilities the adapter announced at initialization.
    #[must_use]
    pub fn capabilities(&self) -> &Value {
        &self.capabilities
    }

    /// Reads whatever the adapter has already sent, without waiting.
    pub fn poll(&mut self) {
        while let Some(message) = self.transport.receive_pending() {
            self.record(&message);
        }
    }

    /// Takes the target output collected so far.
    pub fn take_output(&mut self) -> String {
        self.poll();
        std::mem::take(&mut self.output)
    }

    /// Everything the adapter itself has written to standard error.
    #[must_use]
    pub fn adapter_errors(&self) -> String {
        self.transport.errors()
    }

    /// Sends one request and returns its `body`, pumping events until it lands.
    pub fn request(
        &mut self,
        command: &str,
        arguments: Value,
        timeout: Duration,
    ) -> Result<Value, DapError> {
        let sequence = self.transport.send(command, arguments)?;
        loop {
            let message = self.transport.receive(timeout)?;
            self.record(&message);
            if message.get("type").and_then(Value::as_str) != Some("response")
                || message.get("request_seq").and_then(Value::as_u64) != Some(sequence)
            {
                continue;
            }
            if message.get("success").and_then(Value::as_bool) != Some(true) {
                return Err(DapError::Request {
                    command: command.to_owned(),
                    message: message
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("the adapter gave no reason")
                        .to_owned(),
                });
            }
            return Ok(message.get("body").cloned().unwrap_or(Value::Null));
        }
    }

    /// Waits for the target to stop, or reports the state that ended the wait.
    pub fn wait_for_stop(&mut self, timeout: Duration) -> Result<Stop, DapError> {
        if let TargetState::Stopped(stop) = &self.state {
            return Ok(stop.clone());
        }
        loop {
            let message = self.transport.receive(timeout)?;
            self.record(&message);
            match &self.state {
                TargetState::Stopped(stop) => return Ok(stop.clone()),
                TargetState::Exited(_) | TargetState::Terminated => {
                    return Err(DapError::NotStopped {
                        state: self.state.label(),
                    });
                }
                _ => {}
            }
        }
    }

    /// The thread a stopped-target request applies to.
    pub fn stopped_thread(&self) -> Result<i64, DapError> {
        self.state
            .stop()
            .map(|stop| stop.thread_id)
            .ok_or(DapError::NotStopped {
                state: self.state.label(),
            })
    }

    /// Marks the target as resumed after a request that continues it.
    pub fn mark_running(&mut self) {
        if self.state.is_alive() {
            self.state = TargetState::Running;
        }
    }

    /// Ends the session, terminating the target if it is still alive.
    pub fn disconnect(mut self, terminate: bool) -> (Option<i32>, String) {
        if !self.transport.is_closed() {
            let _ = self.request(
                "disconnect",
                json!({ "terminateDebuggee": terminate }),
                Duration::from_secs(5),
            );
        }
        if self.state.is_alive() && terminate {
            self.transport.terminate();
        }
        self.transport.finish()
    }

    /// Updates session state from one incoming message.
    fn record(&mut self, message: &Value) {
        apply_event(
            message,
            &mut self.state,
            &mut self.output,
            &mut self.capabilities,
        );
    }

    /// Records the capabilities an `initialize` reply carried.
    pub fn set_capabilities(&mut self, capabilities: Value) {
        self.capabilities = capabilities;
    }

    /// Waits for the adapter's `initialized` event and the reply to `sequence`.
    pub fn await_configuration(
        &mut self,
        command: &str,
        arguments: Value,
        timeout: Duration,
    ) -> Result<Value, DapError> {
        let sequence = self.transport.send(command, arguments)?;
        let mut reply = None;
        let mut initialized = false;
        while reply.is_none() || !initialized {
            let message = self.transport.receive(timeout)?;
            self.record(&message);
            if message.get("type").and_then(Value::as_str) == Some("event")
                && message.get("event").and_then(Value::as_str) == Some("initialized")
            {
                initialized = true;
            }
            if message.get("type").and_then(Value::as_str) == Some("response")
                && message.get("request_seq").and_then(Value::as_u64) == Some(sequence)
            {
                if message.get("success").and_then(Value::as_bool) != Some(true) {
                    return Err(DapError::Request {
                        command: command.to_owned(),
                        message: message
                            .get("message")
                            .and_then(Value::as_str)
                            .unwrap_or("the adapter gave no reason")
                            .to_owned(),
                    });
                }
                reply = Some(message.get("body").cloned().unwrap_or(Value::Null));
            }
        }
        Ok(reply.unwrap_or(Value::Null))
    }
}

/// Applies one adapter event to a session's view of its target.
///
/// Separate from the connection so the state machine is exercised directly:
/// every field a caller reads after a resume is decided here.
fn apply_event(
    message: &Value,
    state: &mut TargetState,
    output: &mut String,
    capabilities: &mut Value,
) {
    if message.get("type").and_then(Value::as_str) != Some("event") {
        return;
    }
    let body = message.get("body");
    let field = |name: &str| body.and_then(|body| body.get(name));
    match message.get("event").and_then(Value::as_str) {
        Some("output") => {
            if let Some(text) = field("output").and_then(Value::as_str) {
                output.push_str(text);
            }
        }
        Some("stopped") => {
            *state = TargetState::Stopped(Stop {
                thread_id: field("threadId")
                    .and_then(Value::as_i64)
                    .unwrap_or_default(),
                reason: field("reason")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown")
                    .to_owned(),
                description: field("description")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                hit_breakpoints: field("hitBreakpointIds")
                    .and_then(Value::as_array)
                    .map(|ids| ids.iter().filter_map(Value::as_i64).collect())
                    .unwrap_or_default(),
            });
        }
        Some("continued") => *state = TargetState::Running,
        Some("exited") => {
            let code = field("exitCode")
                .and_then(Value::as_i64)
                .unwrap_or_default();
            *state = TargetState::Exited(code as i32);
        }
        // A target that already reported its status keeps it: `terminated`
        // follows `exited` for an ordinary exit, and an exit code the caller
        // asked for must not be replaced by the fact that the session ended.
        Some("terminated") => {
            if !matches!(state, TargetState::Exited(_)) {
                *state = TargetState::Terminated;
            }
        }
        Some("capabilities") => {
            if let Some(reported) = field("capabilities") {
                *capabilities = reported.clone();
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(name: &str, body: Value) -> Value {
        json!({ "type": "event", "event": name, "body": body })
    }

    struct Session {
        state: TargetState,
        output: String,
        capabilities: Value,
    }

    impl Session {
        fn new() -> Self {
            Self {
                state: TargetState::Configuring,
                output: String::new(),
                capabilities: Value::Null,
            }
        }

        fn apply(&mut self, message: &Value) {
            apply_event(
                message,
                &mut self.state,
                &mut self.output,
                &mut self.capabilities,
            );
        }
    }

    #[test]
    fn a_stop_state_reports_its_thread_and_reason() {
        let mut session = Session::new();
        session.apply(&event(
            "stopped",
            json!({ "reason": "breakpoint", "threadId": 7, "hitBreakpointIds": [2] }),
        ));
        let stop = session.state.stop().expect("a stop");
        assert_eq!(stop.thread_id, 7);
        assert_eq!(stop.reason, "breakpoint");
        assert_eq!(stop.hit_breakpoints, vec![2]);
        assert_eq!(session.state.label(), "stopped");
        assert!(session.state.is_alive());
    }

    #[test]
    fn resuming_clears_the_stop_a_caller_would_otherwise_still_read() {
        let mut session = Session::new();
        session.apply(&event(
            "stopped",
            json!({ "reason": "step", "threadId": 1 }),
        ));
        session.apply(&event("continued", json!({ "threadId": 1 })));
        assert_eq!(session.state, TargetState::Running);
        assert!(session.state.stop().is_none());
    }

    #[test]
    fn target_output_accumulates_across_events() {
        let mut session = Session::new();
        session.apply(&event("output", json!({ "output": "total=" })));
        session.apply(&event("output", json!({ "output": "889\n" })));
        assert_eq!(session.output, "total=889\n");
    }

    /// `terminated` follows `exited`, and the status code is the answer.
    #[test]
    fn an_exit_status_survives_the_termination_that_follows_it() {
        let mut session = Session::new();
        session.apply(&event("exited", json!({ "exitCode": 3 })));
        session.apply(&event("terminated", json!({})));
        assert_eq!(session.state, TargetState::Exited(3));
        assert!(!session.state.is_alive());
    }

    #[test]
    fn a_target_that_ends_without_a_status_is_terminated() {
        let mut session = Session::new();
        session.apply(&event("terminated", json!({})));
        assert_eq!(session.state, TargetState::Terminated);
        assert_eq!(session.state.label(), "terminated");
    }

    #[test]
    fn later_capabilities_replace_the_ones_initialize_announced() {
        let mut session = Session::new();
        session.apply(&event(
            "capabilities",
            json!({ "capabilities": { "supportsDisassembleRequest": true } }),
        ));
        assert_eq!(
            session.capabilities["supportsDisassembleRequest"],
            json!(true)
        );
    }
}
