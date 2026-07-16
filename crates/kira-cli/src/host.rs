//! The standard host: wires the VM's output effect to the process stdout.
//!
//! The VM core is portable and never touches stdout itself; this host lives in
//! the CLI (the native embedder) and supplies the concrete effect. `println!`
//! flushes on each newline, so redirected output is never lost.

use kira_runtime_abi::HostCapabilities;

/// A [`HostCapabilities`] that writes each output line to process stdout.
pub struct StdHost;

impl HostCapabilities for StdHost {
    fn write_line(&mut self, text: &str) {
        println!("{text}");
    }
}
