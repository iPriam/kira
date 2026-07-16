//! Runtime value ABI shared across the VM and native backends.
//!
//! Layer 0 of the Kira package graph.
//!
//! For v0 the only effect a Kira program produces is textual output through
//! `print`. The VM stays a portable core by never touching the outside world
//! directly: it formats values into text internally and pushes finished lines
//! to the embedder through [`HostCapabilities`]. Richer capabilities (clock,
//! rng, native FFI) extend this trait as the language grows; the VM core never
//! gains a filesystem, process, or thread dependency.

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
