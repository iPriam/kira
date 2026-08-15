//! LLDB's Debug Adapter Protocol transport, session, and scripted launch.
//!
//! The command-line LLDB frontend shipped with some Windows toolchains aborts
//! while resuming a second stop. `lldb-dap` uses the same LLDB engine without
//! that command-interpreter path, so Kira can run a real multi-stop session and
//! read the VM's exported state through the standard debugger protocol.

mod client;
mod launch;
mod transport;

pub use client::{DEFAULT_TIMEOUT, DapClient, DapError, Stop, TargetState};
pub use launch::{LldbDapBreakpoint, LldbDapLaunch, decode_base64, parse_address};
pub use transport::TransportError;
