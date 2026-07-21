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
pub mod enum_payload;
pub mod execution;
pub mod foreign;
pub mod ownership;

pub use bridge::{BridgeData, BridgeValue, BridgeValueTag};
pub use enum_payload::EnumPayloadKind;
pub use execution::Execution;
pub use foreign::{
    FOREIGN_ADAPTER_ABI_MARKER, FOREIGN_ADAPTER_ABI_VERSION, FOREIGN_STRING_DATA_SYMBOL,
    FOREIGN_STRING_FREE_SYMBOL, FOREIGN_STRING_LEN_SYMBOL, FOREIGN_STRING_NEW_SYMBOL, ForeignAbi,
    ForeignAdapterFn, ForeignAdapterStatus, ForeignArg, ForeignCallError, ForeignImport,
    ForeignResult, ForeignSignature, ForeignType,
};
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
pub const RUNTIME_ABI_VERSION: u32 = 2;

/// The marker symbol the runtime archive defines and generated code references.
///
/// Its name carries [`RUNTIME_ABI_VERSION`]; a test in `kira-native-bridge`
/// fails if the archive's marker and this name ever drift apart.
pub const RUNTIME_ABI_MARKER: &str = "kira_rt_abi_version_2";

/// The symbols a hybrid host resolves out of a loaded native half by name.
///
/// # Why this list has to exist
///
/// A linker pulls only *referenced* members out of an archive. None of these
/// are referenced by generated code: `kira_hybrid_install_runtime_invoker` is
/// called by the host, and the string helpers are only reached by a program
/// that happens to use strings. So a perfectly good shared library can carry no
/// definition of any of them, and `dlsym` fails on a library that is not broken
/// in any other way.
///
/// The hybrid link step therefore forces each of these in by name, and the host
/// resolves each by name. Both sides read this list rather than spelling the
/// names twice, so the set the linker guarantees and the set the host demands
/// cannot drift apart.
///
/// This is a wire contract: append to it when the host needs to resolve
/// something new, and never remove an entry a released host still resolves.
pub const HYBRID_HOST_SYMBOLS: &[&str] = &[
    "kira_rt_str_new",
    "kira_rt_str_free",
    "kira_rt_str_data",
    "kira_rt_str_len",
    "kira_hybrid_install_runtime_invoker",
];

/// An argument the VM hands to a native function.
///
/// Args **borrow**: a string is a `&str` into the VM's own heap, not a copy, so
/// a runtime-to-native call allocates nothing to make the crossing. That is the
/// Rust model at the seam — and the reason the VM can pass a string it still
/// owns without either side guessing who frees it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum NativeArg<'a> {
    /// The unit value.
    Void,
    /// A 64-bit signed integer.
    Int(i64),
    /// A 64-bit float.
    Float(f64),
    /// A boolean.
    Bool(bool),
    /// A borrowed string, valid for this call only.
    Str(&'a str),
    /// An opaque handle to an object the *caller's* side owns.
    ///
    /// The safe mirror of [`BridgeValueTag::HANDLE`]: one word whose meaning
    /// belongs to whoever minted it. A handle copies like a scalar — passing one
    /// transfers no ownership, which is why it needs no borrow lifetime — and
    /// the object behind it outlives the call either way.
    ///
    /// A receiver that has no way to resolve the word says so with a typed
    /// error. Today the `@Native` seam is such a receiver: handles belong to the
    /// export boundary, and the VM grows a handle representation with the
    /// persistent instance, not here.
    Handle(u64),
    /// An opaque target-width pointer word.
    ///
    /// Kira may store and pass this word back, but never dereferences, performs
    /// arithmetic on, or frees it.
    RawPtr(u64),
}

/// What a native function returned to the VM.
///
/// Results **own**: handing a value out is a move, so the VM takes the string
/// rather than borrowing one whose native storage it does not control.
#[derive(Debug, Clone, PartialEq)]
pub enum NativeResult {
    /// The unit value.
    Void,
    /// A 64-bit signed integer.
    Int(i64),
    /// A 64-bit float.
    Float(f64),
    /// A boolean.
    Bool(bool),
    /// An owned string.
    Str(String),
    /// An opaque handle to an object the *producing* side owns.
    ///
    /// Unlike [`NativeResult::Str`], this is not a move of storage: the object
    /// stays where it was allocated and exactly one generated destructor frees
    /// it. What moves is the right to name it. See [`NativeArg::Handle`].
    Handle(u64),
    /// An opaque target-width pointer word.
    ///
    /// Returning it transfers no ownership and installs no destructor.
    RawPtr(u64),
}

/// Why a call into native code could not be made.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NativeCallError {
    /// This host has no native half; the program is running VM-only.
    NoNativeHalf,
    /// The host has a native half, but nothing bound for this function.
    UnboundFunction(u32),
    /// Native code answered with something this build cannot read.
    MalformedResult(u32),
}

impl core::fmt::Display for NativeCallError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            NativeCallError::NoNativeHalf => write!(
                f,
                "this program called a native function, but the host has no native half \
                 loaded (build it with `--backend hybrid`)"
            ),
            NativeCallError::UnboundFunction(id) => {
                write!(f, "no native symbol is bound for function {id}")
            }
            NativeCallError::MalformedResult(id) => write!(
                f,
                "native function {id} returned a value this runtime cannot read"
            ),
        }
    }
}

/// The effects an embedder grants a running Kira program.
///
/// The VM owns the runtime value representation and all formatting; the host
/// only receives already-rendered lines. This keeps the VM compilable for
/// `wasm32-unknown-unknown`, where the concrete host is supplied by the
/// browser embedder rather than by the standard library.
///
/// The same rule is what makes hybrid possible without breaking the portable
/// core: the VM never dlopens anything or touches a C ABI. When it reaches a
/// call into the native half it asks the embedder, in safe Rust, through
/// [`HostCapabilities::call_native`] — and the embedder, which is native-only
/// by construction, does the marshalling.
pub trait HostCapabilities {
    /// Emits one line of program output (the effect behind the `print` builtin).
    ///
    /// The text is already fully formatted and carries no trailing newline;
    /// the host owns line termination for its destination.
    fn write_line(&mut self, text: &str);

    /// Runs the native function `function_id`, returning what it produced.
    ///
    /// The default refuses: most hosts (the VM-only CLI, the wasm embedder,
    /// tests) have no native half, and a program that reaches this on such a
    /// host is a build error surfacing late, not something to paper over.
    fn call_native(
        &mut self,
        function_id: u32,
        args: &[NativeArg<'_>],
    ) -> Result<NativeResult, NativeCallError> {
        let _ = (function_id, args);
        Err(NativeCallError::NoNativeHalf)
    }

    /// Runs the generated adapter for `foreign_id`.
    ///
    /// The default refuses so the portable VM never acquires a dynamic-loading
    /// dependency and embedders opt into foreign access explicitly.
    fn call_foreign(
        &mut self,
        foreign_id: u32,
        args: &[ForeignArg<'_>],
    ) -> Result<ForeignResult, ForeignCallError> {
        let _ = (foreign_id, args);
        Err(ForeignCallError::NoForeignHost)
    }
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

    #[test]
    fn host_capabilities_refuses_foreign_calls_by_default() {
        let mut host = CapturingHost::new();
        assert_eq!(
            host.call_foreign(0, &[ForeignArg::I32(7)]),
            Err(ForeignCallError::NoForeignHost)
        );
    }
}
