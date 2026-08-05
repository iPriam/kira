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

pub mod aggregate;
pub mod bridge;
pub mod c_storage;
pub mod compiler;
pub mod enum_payload;
pub mod env;
pub mod erased;
pub mod execution;
pub mod file_system;
pub mod foreign;
pub mod native_state;
pub mod ownership;
pub mod string_op;
pub mod tasks;

pub use aggregate::{
    ForeignAggregate, ForeignAggregateError, ForeignAggregateId, ForeignAggregates,
    ForeignArrayElement, ForeignLayout, ForeignLeaf, ForeignMember, ForeignPointerWidth,
    scalar_layout,
};
pub use bridge::{BridgeData, BridgeValue, BridgeValueTag};
pub use compiler::{
    CheckDiagnostic, CheckFile, CheckPackage, CheckRequest, CheckSeverity, CheckWireError,
    CompilerError, CompilerOp, DIAGNOSTIC_FIELDS, PackageChecker,
};
pub use enum_payload::EnumPayloadKind;
pub use env::EnvOp;
pub use erased::ErasedKind;
pub use execution::Execution;
pub use file_system::{FileRequest, FileResponse, FileSystemError, FileSystemHost, FileSystemOp};
pub use foreign::{
    FOREIGN_ADAPTER_ABI_MARKER, FOREIGN_ADAPTER_ABI_VERSION, FOREIGN_STRING_DATA_SYMBOL,
    FOREIGN_STRING_FREE_SYMBOL, FOREIGN_STRING_LEN_SYMBOL, FOREIGN_STRING_NEW_SYMBOL, ForeignAbi,
    ForeignAdapterFn, ForeignAdapterStatus, ForeignArg, ForeignCallError, ForeignCallback,
    ForeignImport, ForeignResult, ForeignSignature, ForeignType, ForeignTypeSpec,
};
pub use native_state::{
    NativeStateError, NativeStateHost, NativeStatePathStep, NativeStateStatus, NativeStateStore,
    NativeStateToken, NativeStateTypeId, NativeStateValue, NativeStateValueTag, native_state_walk,
    native_state_walk_mut,
};
pub use ownership::Ownership;
pub use string_op::StringOp;
pub use tasks::{TASK_SLOTS, TaskExecutor, TaskPrim, TaskTrap};

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
pub const RUNTIME_ABI_VERSION: u32 = 7;

/// Where a string object keeps its share count, as a field index.
///
/// After the `Box<[u8]>` it owns, which is two words wide. A string is never
/// written after it is built, so copying one is a count away from free — and
/// generated code copies strings often enough that the *call* was the cost.
/// The layout test beside `KiraString` is what holds the object to this.
pub const STRING_SHARES_FIELD: u32 = 2;

/// Where an array header keeps its share count, as a field index.
///
/// Copying an array is a share count away from free and releasing one usually
/// is too, and generated code does both often enough that the *call* into the
/// runtime was the cost — so the backend reaches into the header itself. The
/// layout test beside `KiraArray` is what holds the header to this.
pub const ARRAY_HEADER_SHARES_FIELD: u32 = 3;

/// Where an enum box keeps its share count, as a field index.
///
/// Copying and releasing an enum is a share count away from free, and generated
/// code does both often enough that the *call* into the runtime was the cost —
/// so the backend reaches into the box itself. That makes the box's shape a
/// contract between two separately compiled halves like any other: this index
/// is what the backend GEPs with, and `kira_native_bridge::enums`' layout test
/// is what holds the box to it.
pub const ENUM_BOX_SHARES_FIELD: u32 = 3;

/// The marker symbol the runtime archive defines and generated code references.
///
/// Its name carries [`RUNTIME_ABI_VERSION`]; a test in `kira-native-bridge`
/// fails if the archive's marker and this name ever drift apart.
pub const RUNTIME_ABI_MARKER: &str = "kira_rt_abi_version_7";

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
    "kira_rt_heap_report",
    "kira_rt_native_value_int",
    "kira_rt_native_value_raw_ptr",
    "kira_rt_native_value_float",
    "kira_rt_native_value_bool",
    "kira_rt_native_value_string",
    "kira_rt_native_value_aggregate",
    "kira_rt_native_value_set_child",
    "kira_rt_native_value_tag",
    "kira_rt_native_value_read_int",
    "kira_rt_native_value_read_raw_ptr",
    "kira_rt_native_value_read_float",
    "kira_rt_native_value_read_bool",
    "kira_rt_native_value_read_string",
    "kira_rt_native_value_len",
    "kira_rt_native_value_enum_tag",
    "kira_rt_native_value_child",
    "kira_rt_native_value_free",
    "kira_rt_native_state_new",
    "kira_rt_native_state_recover",
    "kira_rt_native_state_replace",
    "kira_rt_native_state_free",
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
    /// A payload-less enum, as its variant tag.
    ///
    /// Copies like a scalar and owns nothing — the whole value is the number —
    /// so it needs no borrow lifetime for the same reason a handle does not.
    /// Each side keeps its enums in its own representation; only the tag
    /// crosses. See [`BridgeValueTag::ENUM`].
    Enum(i64),
    /// A struct, an array, or an enum carrying a payload, as a value tree.
    ///
    /// Borrowed for the call, exactly as a string is: the tree is copied into
    /// the other side's own representation before the callee runs, and this
    /// side keeps its own. See [`BridgeValueTag::NODE`].
    Aggregate(&'a NativeStateValue),
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
    /// A payload-less enum, as its variant tag.
    ///
    /// Unlike [`NativeResult::Str`] this moves no storage: there is none to
    /// move. The receiver builds its own value from the number.
    Enum(i64),
    /// A struct, an array, or an enum carrying a payload, as a value tree.
    ///
    /// Owned, like [`NativeResult::Str`]: the tree was decoded out of what the
    /// other side handed over, and that copy is now this side's.
    Aggregate(NativeStateValue),
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

    /// The address C calls to enter the Kira function callback `callback_id`
    /// names.
    ///
    /// The VM has no native code of its own, so the pointer a `@FFI.Callback`
    /// value carries is one the host produced — the entry thunk in the generated
    /// sidecar. The default refuses for the same reason [`Self::call_foreign`]
    /// does: a portable VM acquires no dynamic-loading dependency, and an
    /// embedder opts into foreign access explicitly.
    fn foreign_callback(&mut self, callback_id: u32) -> Result<u64, ForeignCallError> {
        let _ = callback_id;
        Err(ForeignCallError::NoForeignHost)
    }

    /// Boxes a backend-neutral Kira value in stable callback-state storage.
    fn native_state_create(
        &mut self,
        ty: NativeStateTypeId,
        value: NativeStateValue,
    ) -> Result<NativeStateToken, NativeStateError> {
        let _ = (ty, value);
        Err(NativeStateError::NoStateHost)
    }

    /// Recovers an owned copy of callback state after validating its type.
    fn native_state_recover(
        &mut self,
        token: NativeStateToken,
        ty: NativeStateTypeId,
    ) -> Result<NativeStateValue, NativeStateError> {
        let _ = (token, ty);
        Err(NativeStateError::NoStateHost)
    }

    /// Replaces callback state after validating its token and type.
    fn native_state_replace(
        &mut self,
        token: NativeStateToken,
        ty: NativeStateTypeId,
        value: NativeStateValue,
    ) -> Result<(), NativeStateError> {
        let _ = (token, ty, value);
        Err(NativeStateError::NoStateHost)
    }

    /// Checks that a token names live state of this type, reading nothing.
    ///
    /// `nativeRecover` needs the type check and nothing else — it hands back a
    /// handle, not a copy. The default answers by recovering, which deep-copies
    /// the whole state and discards it: a host that owns its storage should
    /// override this, or every recovery pays for the state's entire contents.
    fn native_state_check(
        &mut self,
        token: NativeStateToken,
        ty: NativeStateTypeId,
    ) -> Result<(), NativeStateError> {
        self.native_state_recover(token, ty).map(|_| ())
    }

    /// Reads one value out of callback state, addressed by path.
    ///
    /// The default recovers the whole state and walks it, which is what
    /// [`Self::native_state_recover`] costs. A host that owns its storage
    /// should override this: reading one integer field is otherwise a deep copy
    /// of everything the state holds, and a UI batch holding a glyph cache pays
    /// that on every field access of every frame.
    fn native_state_read(
        &mut self,
        token: NativeStateToken,
        ty: NativeStateTypeId,
        path: &[NativeStatePathStep],
    ) -> Result<NativeStateValue, NativeStateError> {
        let root = self.native_state_recover(token, ty)?;
        native_state_walk(&root, path).cloned()
    }

    /// Writes one value into callback state, addressed by path.
    ///
    /// The default recovers, walks, writes, and replaces — two deep copies of
    /// the whole state per field write. Overriding it is the difference between
    /// a field assignment costing the state's size and costing its depth.
    fn native_state_write(
        &mut self,
        token: NativeStateToken,
        ty: NativeStateTypeId,
        path: &[NativeStatePathStep],
        value: NativeStateValue,
    ) -> Result<(), NativeStateError> {
        let mut root = self.native_state_recover(token, ty)?;
        *native_state_walk_mut(&mut root, path)? = value;
        self.native_state_replace(token, ty, root)
    }

    /// Appends one element to an array inside callback state, addressed by path.
    fn native_state_append(
        &mut self,
        token: NativeStateToken,
        ty: NativeStateTypeId,
        path: &[NativeStatePathStep],
        value: NativeStateValue,
    ) -> Result<(), NativeStateError> {
        let mut root = self.native_state_recover(token, ty)?;
        match native_state_walk_mut(&mut root, path)? {
            // The elements are shared with whoever last read this array, so the
            // append buys a block of its own before it lands.
            NativeStateValue::Array(elements) => std::sync::Arc::make_mut(elements).push(value),
            _ => return Err(NativeStateError::PathMismatch),
        }
        self.native_state_replace(token, ty, root)
    }

    /// Releases callback state exactly once.
    fn native_state_free(&mut self, token: NativeStateToken) -> Result<(), NativeStateError> {
        let _ = token;
        Err(NativeStateError::NoStateHost)
    }

    /// Performs one file-system operation on the embedder's behalf.
    ///
    /// The default refuses, for the same reason [`Self::call_foreign`] does: the
    /// VM core reaches nothing outside itself, so an embedder — a browser tab, a
    /// test, a sandbox — grants filesystem access explicitly by wrapping its
    /// host in [`FileSystemHost`] or implementing this itself.
    ///
    /// A *failed* operation is not an error here: a missing file answers
    /// [`FileResponse::Flag(false)`](FileResponse::Flag) or an empty result. The
    /// error is only for a host with no filesystem at all.
    fn file_system(&mut self, request: FileRequest<'_>) -> Result<FileResponse, FileSystemError> {
        let _ = request;
        Err(FileSystemError::NoFileSystemHost)
    }

    /// Checks a package set the program built in memory, answering with its
    /// diagnostics.
    ///
    /// The default answers through the compiler the embedder installed with
    /// [`compiler::install`], and refuses when it installed none. That is the
    /// same arrangement [`Self::file_system`] has with
    /// [`file_system::perform`] and it is what the VM's position in the
    /// layering forces: the VM sits *below* the compiler and can never hold
    /// one, so a build that contains a frontend has to hand it in. Every other
    /// host — a browser tab, a test, a sandbox — refuses by name instead of
    /// answering with an empty diagnostic list that would read as success.
    ///
    /// A package that does not compile is not an error here: its problems are
    /// the answer. The error is for a host with no compiler at all, and for a
    /// request that could not be read.
    fn compiler(&mut self, request: &CheckRequest) -> Result<Vec<CheckDiagnostic>, CompilerError> {
        compiler::perform(request)
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

    /// A host that was never given a compiler says so, rather than answering
    /// "no diagnostics" — which a caller would read as "it compiled".
    #[test]
    fn host_capabilities_refuses_the_compiler_by_default() {
        let mut host = CapturingHost::new();
        assert_eq!(
            host.compiler(&CheckRequest::default()),
            Err(CompilerError::NoCompilerHost)
        );
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
