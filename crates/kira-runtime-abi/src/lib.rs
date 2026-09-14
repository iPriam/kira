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
pub mod channels;
pub mod compiler;
pub mod enum_payload;
pub mod env;
pub mod erased;
pub mod execution;
pub mod file_system;
pub mod foreign;
pub mod int_width;
pub mod main_thread;
pub mod math_op;
pub mod native_state;
pub mod ownership;
pub mod number;
pub mod number_op;
pub mod string_op;
pub mod syscall;
pub mod tasks;
pub mod toolchain;

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
pub use int_width::IntWidth;
pub use toolchain::{
    TOOL_DIAGNOSTIC_FIELDS, ToolAnswer, ToolBackend, ToolDiagnostic, ToolRequest, ToolVariable,
    ToolVerb, ToolWireError, Toolchain, ToolchainError,
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

/// Returns the exported symbol for foreign import `index`.
///
/// The adapter sidecar producer and every native-capable host use this same
/// name, so an import id cannot bind a different adapter after a bundle crosses
/// the process boundary.
pub fn foreign_adapter_name(index: usize) -> String {
    format!("kira_foreign_adapter_{index}")
}

/// Returns the exported callback entry symbol for callback `index`.
///
/// The adapter sidecar producer and the VM host use this same name when C
/// calls back into a VM function.
pub fn foreign_callback_name(index: usize) -> String {
    format!("kira_ffi_callback_{index}")
}
pub use channels::{BOXED_PAYLOAD, ChannelExecutor, ChannelPrim, ChannelReceive, ChannelTrap};
pub use main_thread::*;
pub use math_op::MathOp;
pub use native_state::{
    CBlockOffset, NativeCBlock, NativeCBlockChild, NativeCBlockError, NativeCell, NativeStateError,
    NativeStateHost, NativeStateOwner, NativeStatePathStep, NativeStateStatus, NativeStateStore,
    NativeStateToken, NativeStateTypeId, NativeStateValue, NativeStateValueTag, native_state_walk,
    native_state_walk_mut,
};
pub use ownership::Ownership;
pub use number::{Decimal, DecimalError};
pub use number_op::NumberOp;
pub use string_op::StringOp;
pub use syscall::{
    LINUX_SYSCALLS, LinuxSyscall, MAX_SYSCALL_ARGUMENTS, SYSCALL_OS, SyscallArch, SyscallError,
};
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
pub const RUNTIME_ABI_VERSION: u32 = 18;

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
pub const RUNTIME_ABI_MARKER: &str = "kira_rt_abi_version_18";

/// The fixed C symbol exported by a whole-program native live library.
///
/// Its signature is `unsafe extern "C" fn() -> i32`: the runner loads this
/// symbol after staging the library and invokes it in the runner process.
pub const NATIVE_LIVE_ENTRY_SYMBOL: &str = "kira_live_entry";

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
    "kira_live_mark_reload",
    "kira_live_take_reload",
    "kira_rt_heap_report",
    "kira_rt_task_reset",
    "kira_rt_native_value_int",
    "kira_rt_native_value_number",
    "kira_rt_native_value_any",
    "kira_rt_native_value_read_any_type",
    "kira_rt_native_value_cell",
    "kira_rt_native_value_read_cell",
    "kira_rt_cell_free",
    "kira_rt_cell_vm_proxy_new",
    "kira_rt_cell_vm_proxy_handle",
    "kira_rt_native_value_raw_ptr",
    "kira_rt_native_value_float",
    "kira_rt_native_value_bool",
    "kira_rt_native_value_string",
    "kira_rt_native_value_aggregate",
    "kira_rt_native_value_set_child",
    "kira_rt_native_value_tag",
    "kira_rt_native_value_read_int",
    "kira_rt_native_value_read_number",
    "kira_rt_native_value_read_raw_ptr",
    "kira_rt_native_value_read_float",
    "kira_rt_native_value_read_bool",
    "kira_rt_native_value_read_string",
    "kira_rt_native_value_len",
    "kira_rt_native_value_enum_tag",
    "kira_rt_native_value_child",
    "kira_rt_native_value_cblock",
    "kira_rt_native_value_read_cblock_len",
    "kira_rt_native_value_read_cblock_data",
    "kira_rt_native_value_free",
    "kira_rt_native_state_new",
    "kira_rt_native_state_new_dropping",
    "kira_rt_native_state_recover",
    "kira_rt_native_state_replace",
    "kira_rt_native_state_free",
    "kira_rt_native_state_retain",
    "kira_rt_native_state_release",
    "kira_rt_native_state_release_dropping",
    "kira_rt_native_value_set_cblock_child",
    "kira_rt_native_value_cblock_child_offset",
    "kira_rt_native_value_cblock_child_width",
    "kira_rt_native_value_cblock_from_handle",
    "kira_rt_native_value_cblock_to_handle",
    "kira_rt_cblock_release_retained",
    "kira_rt_main_thread_call",
    "kira_rt_main_thread_join",
    "kira_rt_main_thread_run",
    "kira_rt_main_thread_install_dispatcher",
    "kira_main_thread_dispatch",
    "kira_rt_main_thread_install_lifecycle_resolver",
    "kira_main_thread_lifecycle_resolve",
    "kira_rt_main_thread_lifecycle_start_local",
    "kira_rt_main_thread_lifecycle_pump_local",
    "kira_rt_main_thread_lifecycle_reset_local",
    "kira_rt_channel_reset",
    "kira_rt_channel_try",
    "kira_rt_native_value_native_state",
    "kira_rt_native_value_read_native_state",
    "kira_rt_native_value_read_drop_glue",
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
    /// A struct, an array, an enum carrying a payload, or an `Any` value, as a
    /// value tree.
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
    /// One affine callback-state owner crossing the runtime/native seam.
    NativeState(u64),
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
    /// A struct, an array, an enum carrying a payload, or an `Any` value, as a
    /// value tree.
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
    /// One affine callback-state owner transferred to the receiving engine.
    NativeState(u64),
}

/// What a native call produced: its result, and any parameter it wrote through.
///
/// The two travel together because they are the same crossing. A `borrow mut`
/// parameter is the one place a callee's effect is not its return value, and the
/// two engines do not share a heap — so the caller cannot see the write, and the
/// final value has to come back the way the result does.
#[derive(Debug, Clone, PartialEq)]
pub struct NativeReturn {
    /// What the function returned.
    pub result: NativeResult,
    /// The final value of each parameter the callee was allowed to write
    /// through, by parameter slot, ascending.
    ///
    /// Empty for the overwhelming majority of calls: only a `borrow mut`
    /// parameter appears here, and only a signature that declares one can have
    /// any. A caller matches these against the writebacks its own call site
    /// recorded — the two come from one IR, and a disagreement is a broken
    /// artifact rather than something to reconcile at runtime.
    pub writebacks: Vec<(u32, NativeResult)>,
}

impl NativeReturn {
    /// A call that returned `result` and wrote through nothing.
    pub fn plain(result: NativeResult) -> NativeReturn {
        NativeReturn {
            result,
            writebacks: Vec::new(),
        }
    }
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

mod host;

pub use host::{CapturingHost, HostCapabilities};
