//! Runtime value ABI shared across the VM and native backends.
//!
//! Layer 0 of the Kira package graph.
//!
//! This crate owns three contracts, each defined here exactly once because
//! everything from the parser to the hybrid runtime shares them:
//!
//! - [`HostCapabilities`], the effects an embedder grants a running program,
//! - [`Execution`], where a function's body runs (`@Runtime` / `@Native`),
//! - [`BridgeValue`], how one value crosses the runtime/native boundary.
//!
//! For v0 the only effect a Kira program produces is textual output through
//! `print`. The VM stays a portable core by never touching the outside world
//! directly: it formats values into text internally and pushes finished lines
//! to the embedder through [`HostCapabilities`]. Richer capabilities (clock,
//! rng, native FFI) extend this trait as the language grows; the VM core never
//! gains a filesystem, process, or thread dependency.

pub mod bridge;
pub mod execution;
pub mod ownership;

pub use bridge::{BridgeData, BridgeValue, BridgeValueTag};
pub use execution::Execution;
pub use ownership::Ownership;

/// The version of the `kira_rt_*` native runtime contract.
///
/// Bump this on **any** change to a `kira_rt_*` signature, to what a helper
/// owns or frees, or to how a value is represented at the native ABI.
///
/// # Why a version exists at all
///
/// Generated native code and the runtime archive are built separately and
/// linked together. If they disagree — an archive built before a signature
/// changed — the symbols still resolve by name and the mismatch is silent: the
/// program calls the old code with the new ABI and corrupts memory. That is the
/// worst failure mode available.
///
/// So the version is baked into a symbol name ([`RUNTIME_ABI_MARKER`]) that the
/// backend emits a reference to. A stale archive does not define this version's
/// marker, so the link fails by name instead of the program failing at runtime.
pub const RUNTIME_ABI_VERSION: u32 = 1;

/// The marker symbol the runtime archive defines and generated code references.
///
/// Its name carries [`RUNTIME_ABI_VERSION`]; a test in `kira-native-bridge`
/// fails if the archive's marker and this name ever drift apart.
pub const RUNTIME_ABI_MARKER: &str = "kira_rt_abi_version_1";

/// The effects an embedder grants a running Kira program.
///
/// The VM owns the runtime value representation and all formatting; the host
/// only receives already-rendered lines. This keeps the VM compilable for
/// `wasm32-unknown-unknown`, where the concrete host is supplied by the
/// browser embedder rather than by the standard library.
pub trait HostCapabilities {
    /// Emits one line of program output (the effect behind the `print` builtin).
    ///
    /// The text is already fully formatted and carries no trailing newline;
    /// the host owns line termination for its destination.
    fn write_line(&mut self, text: &str);
}

/// A [`HostCapabilities`] implementation that records every line in memory.
///
/// Useful for tests and for embedders that want to capture output rather than
/// stream it. Ships in the portable core because it needs nothing but `alloc`.
#[derive(Debug, Default)]
pub struct CapturingHost {
    lines: Vec<String>,
}

impl CapturingHost {
    /// Creates a host with no captured output.
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns every line captured so far, in emission order.
    pub fn lines(&self) -> &[String] {
        &self.lines
    }

    /// Renders all captured lines back into a single newline-terminated string.
    pub fn into_output(self) -> String {
        let mut out = String::new();
        for line in self.lines {
            out.push_str(&line);
            out.push('\n');
        }
        out
    }
}

impl HostCapabilities for CapturingHost {
    fn write_line(&mut self, text: &str) {
        self.lines.push(text.to_owned());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capturing_host_records_lines_in_order() {
        let mut host = CapturingHost::new();
        host.write_line("first");
        host.write_line("second");
        assert_eq!(host.lines(), ["first".to_owned(), "second".to_owned()]);
        assert_eq!(host.into_output(), "first\nsecond\n");
    }
}
