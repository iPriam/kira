//! The effects an embedded Kira library gets when the embedder supplies none.
//!
//! The VM is a portable core: it formats `print` output into finished lines and
//! hands them to a [`HostCapabilities`], never touching the outside world
//! itself. Someone has to be that host. For an *application* it is the CLI; for
//! a library embedded in a Rust program there is no obvious answer, so this
//! crate picks the one that surprises nobody — stdout — and makes it entirely
//! replaceable.
//!
//! Replacing it is the point, not an escape hatch. A Rust program that embeds a
//! UI library and wants its `print` in a log, a test buffer, or a browser
//! console supplies its own host to
//! [`Library::instantiate_with`](crate::Library::instantiate_with) and nothing
//! else about the library changes.
//! [`CapturingHost`](kira_runtime_abi::CapturingHost) is the one every test
//! here uses.

use kira_runtime_abi::{FileRequest, FileResponse, FileSystemError, HostCapabilities, file_system};

/// A host that writes each line of program output to process stdout.
///
/// The default an [`Instance`](crate::Instance) gets from
/// [`Library::instantiate`](crate::Library::instantiate). `println!` flushes on
/// each newline, so redirected output is never lost.
///
/// This compiles for `wasm32-unknown-unknown` — where the target simply has no
/// stdout to reach and the lines go nowhere — which is what keeps the VM-engine
/// wrapper crate buildable for the web without a second default.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct StdoutHost;

impl HostCapabilities for StdoutHost {
    fn write_line(&mut self, text: &str) {
        println!("{text}");
    }

    /// Reaches the process's own filesystem.
    ///
    /// The same grant `write_line` already makes, in the other direction: a host
    /// that writes to this process's stdout is a host standing in this process,
    /// and a program running under it reads the files this process can read. An
    /// embedder that wants a narrower world supplies its own host, exactly as it
    /// does to redirect output.
    ///
    /// On `wasm32-unknown-unknown` there is no filesystem behind this, and every
    /// operation answers the way a missing file does — which is an answer, not a
    /// crash, so a program that asks still runs.
    fn file_system(&mut self, request: FileRequest<'_>) -> Result<FileResponse, FileSystemError> {
        Ok(file_system::perform(request))
    }
}
