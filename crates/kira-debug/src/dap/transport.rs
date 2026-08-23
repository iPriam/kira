//! The Debug Adapter Protocol wire format over a child process's pipes.
//!
//! Reading is done on its own thread and delivered over a channel. A debug
//! adapter sends events whenever it likes — the target wrote to its output, a
//! thread stopped, the process exited — so a caller that read the pipe only
//! while waiting for a reply would block on whichever message arrived first.
//! The thread also makes a timeout possible: a blocking pipe read cannot be
//! cancelled, and an interactive caller must never lose its session to an
//! adapter that stops answering.

use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, ChildStderr, ChildStdin, ChildStdout};
use std::sync::mpsc::{Receiver, RecvTimeoutError, TryRecvError, channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};

/// One decoded message from the adapter, or the end of its output.
#[derive(Debug)]
enum Incoming {
    Message(Value),
    Closed(String),
}

/// Why a transport operation did not complete.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransportError {
    /// The adapter did not answer within the caller's deadline.
    Timeout,
    /// The adapter closed its output, with the reason the reader saw.
    Closed(String),
    /// A message could not be written to or decoded from the pipe.
    Protocol(String),
}

impl std::fmt::Display for TransportError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Timeout => write!(formatter, "the debug adapter did not answer in time"),
            Self::Closed(reason) => write!(formatter, "the debug adapter closed: {reason}"),
            Self::Protocol(message) => write!(formatter, "debug adapter protocol error: {message}"),
        }
    }
}

/// A framed connection to a running debug adapter.
pub struct Transport {
    child: Child,
    /// The request pipe, until an orderly close gives it back to the adapter.
    ///
    /// An option because [`Transport::finish`] closes it by taking it, and a
    /// type with a `Drop` impl cannot be partially moved out of.
    input: Option<ChildStdin>,
    incoming: Receiver<Incoming>,
    errors: Arc<Mutex<String>>,
    next_sequence: u64,
    closed: Option<String>,
}

impl Transport {
    /// Takes ownership of a spawned adapter and starts reading it.
    pub fn new(mut child: Child) -> Result<Self, (Child, String)> {
        let Some(input) = child.stdin.take() else {
            return Err((child, "the debug adapter exposed no stdin".to_owned()));
        };
        let Some(output) = child.stdout.take() else {
            return Err((child, "the debug adapter exposed no stdout".to_owned()));
        };
        let Some(error) = child.stderr.take() else {
            return Err((child, "the debug adapter exposed no stderr".to_owned()));
        };
        let (sender, incoming) = channel();
        std::thread::spawn(move || read_messages(output, &sender));
        let errors = Arc::new(Mutex::new(String::new()));
        let collected = Arc::clone(&errors);
        std::thread::spawn(move || collect_errors(error, &collected));
        Ok(Self {
            child,
            input: Some(input),
            incoming,
            errors,
            next_sequence: 1,
            closed: None,
        })
    }

    /// Sends one request, returning the sequence number its reply will carry.
    pub fn send(&mut self, command: &str, arguments: Value) -> Result<u64, TransportError> {
        let sequence = self.next_sequence;
        self.next_sequence += 1;
        let message = json!({
            "seq": sequence,
            "type": "request",
            "command": command,
            "arguments": arguments,
        });
        let body = serde_json::to_vec(&message)
            .map_err(|error| TransportError::Protocol(error.to_string()))?;
        let Some(input) = self.input.as_mut() else {
            return Err(TransportError::Protocol(
                "the request pipe was already closed".to_owned(),
            ));
        };
        write!(input, "Content-Length: {}\r\n\r\n", body.len())
            .and_then(|()| input.write_all(&body))
            .and_then(|()| input.flush())
            .map_err(|error| TransportError::Closed(error.to_string()))?;
        Ok(sequence)
    }

    /// Receives the next message, waiting at most `timeout`.
    pub fn receive(&mut self, timeout: Duration) -> Result<Value, TransportError> {
        if let Some(reason) = &self.closed {
            return Err(TransportError::Closed(reason.clone()));
        }
        match self.incoming.recv_timeout(timeout) {
            Ok(Incoming::Message(message)) => Ok(message),
            Ok(Incoming::Closed(reason)) => Err(self.close(reason)),
            Err(RecvTimeoutError::Timeout) => Err(TransportError::Timeout),
            Err(RecvTimeoutError::Disconnected) => Err(self.close("the reader stopped".to_owned())),
        }
    }

    /// Receives a message that has already arrived, without waiting.
    pub fn receive_pending(&mut self) -> Option<Value> {
        if self.closed.is_some() {
            return None;
        }
        match self.incoming.try_recv() {
            Ok(Incoming::Message(message)) => Some(message),
            Ok(Incoming::Closed(reason)) => {
                self.close(reason);
                None
            }
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => {
                self.close("the reader stopped".to_owned());
                None
            }
        }
    }

    /// Everything the adapter has written to standard error so far.
    pub fn errors(&self) -> String {
        self.errors
            .lock()
            .map(|errors| errors.clone())
            .unwrap_or_default()
    }

    /// Whether the adapter's output has ended.
    pub fn is_closed(&self) -> bool {
        self.closed.is_some()
    }

    /// Kills the adapter process and waits for it, returning its exit code.
    pub fn terminate(&mut self) -> Option<i32> {
        let _ = self.child.kill();
        self.child.wait().ok().and_then(|status| status.code())
    }

    /// Closes the request pipe and waits for the adapter to exit on its own.
    ///
    /// The wait is bounded: an adapter that ignores stdin EOF — one wedged
    /// inside a live debuggee, say — would otherwise hold this caller forever,
    /// and the synchronous servers above this have no other thread to notice
    /// with. Past the grace period the adapter is killed so the wait ends.
    pub fn finish(mut self) -> (Option<i32>, String) {
        drop(self.input.take());
        let code = self
            .wait_for_exit(ADAPTER_EXIT_GRACE)
            .ok()
            .and_then(|status| status.code())
            .or_else(|| {
                let _ = self.child.kill();
                self.child.wait().ok().and_then(|status| status.code())
            });
        let collected = self
            .errors
            .lock()
            .map(|errors| errors.clone())
            .unwrap_or_default();
        (code, collected)
    }

    /// Waits up to `limit` for the child to exit on its own.
    fn wait_for_exit(&mut self, limit: Duration) -> std::io::Result<std::process::ExitStatus> {
        let started = std::time::Instant::now();
        loop {
            match self.child.try_wait()? {
                Some(status) => return Ok(status),
                None if started.elapsed() >= limit => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::WouldBlock,
                        "the debug adapter did not exit in time",
                    ));
                }
                None => std::thread::sleep(Duration::from_millis(25)),
            }
        }
    }

    fn close(&mut self, reason: String) -> TransportError {
        self.closed = Some(reason.clone());
        TransportError::Closed(reason)
    }
}

/// How long [`Transport::finish`] waits for an orderly exit before killing.
const ADAPTER_EXIT_GRACE: Duration = Duration::from_secs(5);

impl Drop for Transport {
    fn drop(&mut self) {
        // A session that never reached an orderly close — a failed launch
        // between spawn and `configurationDone`, a dropped client — still owns
        // this child. Leaving it running leaks the adapter, its reader threads,
        // and every debuggee it already started; killing here costs nothing on
        // paths where `terminate` or `finish` ran first, because those leave
        // the child reaped and a second kill/wait is a no-op.
        if self.child.try_wait().is_ok_and(|status| status.is_none()) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

/// Reads framed messages until the adapter's output ends.
fn read_messages(output: ChildStdout, sender: &std::sync::mpsc::Sender<Incoming>) {
    let mut reader = BufReader::new(output);
    loop {
        match read_message(&mut reader) {
            Ok(message) => {
                if sender.send(Incoming::Message(message)).is_err() {
                    return;
                }
            }
            Err(reason) => {
                let _ = sender.send(Incoming::Closed(reason));
                return;
            }
        }
    }
}

/// Reads one `Content-Length`-framed message.
fn read_message(reader: &mut BufReader<ChildStdout>) -> Result<Value, String> {
    let mut content_length = None;
    loop {
        let mut line = String::new();
        let read = reader
            .read_line(&mut line)
            .map_err(|error| error.to_string())?;
        if read == 0 {
            return Err("the adapter ended its output".to_owned());
        }
        if line == "\r\n" || line == "\n" {
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
    let length = content_length.ok_or_else(|| "a message carried no content length".to_owned())?;
    let mut body = vec![0; length];
    reader
        .read_exact(&mut body)
        .map_err(|error| error.to_string())?;
    serde_json::from_slice(&body).map_err(|error| error.to_string())
}

/// Accumulates the adapter's standard error for a later report.
fn collect_errors(mut error: ChildStderr, collected: &Arc<Mutex<String>>) {
    let mut buffer = [0_u8; 1024];
    loop {
        match error.read(&mut buffer) {
            Ok(0) | Err(_) => return,
            Ok(read) => {
                let text = String::from_utf8_lossy(&buffer[..read]);
                if let Ok(mut collected) = collected.lock() {
                    collected.push_str(&text);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_failure_says_which_side_gave_up() {
        assert_eq!(
            TransportError::Timeout.to_string(),
            "the debug adapter did not answer in time"
        );
        assert_eq!(
            TransportError::Closed("exited".to_owned()).to_string(),
            "the debug adapter closed: exited"
        );
        assert_eq!(
            TransportError::Protocol("bad json".to_owned()).to_string(),
            "debug adapter protocol error: bad json"
        );
    }
}
