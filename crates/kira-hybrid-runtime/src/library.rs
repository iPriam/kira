//! The loaded native half: `dlopen`, symbol binding, and the string helpers the
//! host reaches the library's heap through.
//!
//! # Why nothing here links against the runtime
//!
//! This crate deliberately does not depend on `kira-native-bridge`. The process
//! hosting a hybrid program almost certainly carries its own copy of the
//! `kira_rt_*` symbols already — `kira` links the runtime as an rlib so cargo
//! also builds the staticlib the compiler links — and a string handle allocated
//! by one copy and freed by the other is a cross-allocator free.
//!
//! So every symbol is resolved out of the *loaded library* by name.
//! `libloading` opens with `RTLD_LOCAL`, so `dlsym` on the returned handle
//! searches that library rather than the global namespace, which is what keeps
//! the two copies apart. Never resolve one of these by calling it directly.

use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use kira_hybrid_definition::HybridFunction;
use kira_runtime_abi::{
    BridgeValue, NativeStateError, NativeStateStatus, NativeStateToken, NativeStateTypeId,
    NativeStateValue, NativeStateValueTag,
};

use crate::error::HybridError;

/// A native function's trampoline: `kira_native_fn_<id>`.
///
/// Every native function is reached through one of these regardless of its own
/// signature, so the host marshals into one shape instead of building a call
/// per signature.
///
/// `args` is written as well as read. A parameter the callee writes through has
/// its final value packed back into the slot it arrived in — the two engines
/// share no heap, so a `borrow mut` crosses as a copy out and a copy back, and
/// the argument array is where the return trip lands.
///
/// # Safety
/// `args` must point at `count` readable *and writable* [`BridgeValue`]s (or be
/// null when `count` is 0), and `out` must point at one writable
/// [`BridgeValue`].
pub type TrampolineFn =
    unsafe extern "C" fn(args: *mut BridgeValue, count: u32, out: *mut BridgeValue);

/// The host callback the library calls to run a `@Runtime` function.
///
/// Mirrors `kira_hybrid_call_runtime`'s invoker type in `kira-native-bridge`.
/// It is spelled out again here rather than imported for the reason in this
/// module's docs: the host must not link the runtime.
///
/// # Safety
/// The same contract as [`TrampolineFn`], plus a `function_id` naming a
/// `@Runtime` function.
pub type RuntimeInvoker = unsafe extern "C" fn(
    function_id: u32,
    args: *mut BridgeValue,
    count: u32,
    out: *mut BridgeValue,
);

/// An opaque `KStr` handle into the loaded library's string heap.
///
/// A `u64` rather than a newtype because that is the contract's own spelling:
/// [`kira_runtime_abi::BridgeData::String`] carries the handle as a `u64`, and
/// this is the same handle. It is opaque here — this crate never dereferences
/// one, and could not: `KiraString` is private to the runtime.
pub type StrHandle = u64;

type StrNewFn = unsafe extern "C" fn(data: *const u8, len: usize) -> *mut c_void;
type StrFreeFn = unsafe extern "C" fn(value: *mut c_void);
type StrDataFn = unsafe extern "C" fn(value: *mut c_void) -> *const u8;
type StrLenFn = unsafe extern "C" fn(value: *mut c_void) -> usize;
type HeapReportFn = unsafe extern "C" fn();
type LiveReloadMarkFn = unsafe extern "C" fn();
type InstallInvokerFn = unsafe extern "C" fn(invoker: Option<RuntimeInvoker>);
type StateNode = *mut c_void;
type StateIntFn = unsafe extern "C" fn(i64) -> StateNode;
type StateAnyFn = unsafe extern "C" fn(u64, StateNode) -> StateNode;
type StateReadAnyTypeFn = unsafe extern "C" fn(StateNode) -> u64;
type StateRawPtrFn = unsafe extern "C" fn(u64) -> StateNode;
type StateCellFn = unsafe extern "C" fn(u64) -> StateNode;
type StateReadCellFn = unsafe extern "C" fn(StateNode) -> u64;
type CellFreeFn = unsafe extern "C" fn(u64);
type CellProxyNewFn = unsafe extern "C" fn(u64) -> u64;
type CellProxyHandleFn = unsafe extern "C" fn(u64) -> u64;
type StateFloatFn = unsafe extern "C" fn(f64) -> StateNode;
type StateBoolFn = unsafe extern "C" fn(u8) -> StateNode;
type StateStringFn = unsafe extern "C" fn(*mut c_void) -> StateNode;
type StateAggregateFn = unsafe extern "C" fn(u32, u32, usize) -> StateNode;
type StateSetChildFn = unsafe extern "C" fn(StateNode, usize, StateNode) -> u32;
type StateTagFn = unsafe extern "C" fn(StateNode) -> u32;
type StateReadIntFn = unsafe extern "C" fn(StateNode) -> i64;
type StateReadRawPtrFn = unsafe extern "C" fn(StateNode) -> u64;
type StateReadFloatFn = unsafe extern "C" fn(StateNode) -> f64;
type StateReadBoolFn = unsafe extern "C" fn(StateNode) -> u8;
type StateReadStringFn = unsafe extern "C" fn(StateNode) -> *mut c_void;
type StateLenFn = unsafe extern "C" fn(StateNode) -> usize;
type StateEnumTagFn = unsafe extern "C" fn(StateNode) -> u32;
type StateChildFn = unsafe extern "C" fn(StateNode, usize) -> StateNode;
type StateNodeFreeFn = unsafe extern "C" fn(StateNode);
type StateNewFn = unsafe extern "C" fn(u64, StateNode, *mut u64) -> u32;
type StateRecoverFn = unsafe extern "C" fn(u64, u64, *mut StateNode) -> u32;
type StateReplaceFn = unsafe extern "C" fn(u64, u64, StateNode) -> u32;
type StateFreeFn = unsafe extern "C" fn(u64) -> u32;
/// The loaded library lease carried by a decoded cell's release closure.
struct CellReleaseOwner<L> {
    /// Keeps the image containing `free` loaded until all cell shares release.
    _library: Arc<L>,
    /// Resolved from the library held by `_library`.
    free: CellFreeFn,
}

impl<L> CellReleaseOwner<L> {
    fn new(library: Arc<L>, free: CellFreeFn) -> CellReleaseOwner<L> {
        CellReleaseOwner {
            _library: library,
            free,
        }
    }

    fn release(&self, handle: u64) {
        // SAFETY: `free` came from the library held by this owner, and the
        // owner remains alive for the duration of the call.
        unsafe { (self.free)(handle) };
    }
}

/// The symbols every hybrid library must export, whatever the program does.
const STR_NEW: &[u8] = b"kira_rt_str_new\0";
const STR_FREE: &[u8] = b"kira_rt_str_free\0";
const STR_DATA: &[u8] = b"kira_rt_str_data\0";
const STR_LEN: &[u8] = b"kira_rt_str_len\0";
const INSTALL_INVOKER: &[u8] = b"kira_hybrid_install_runtime_invoker\0";
const LIVE_RELOAD_MARK: &[u8] = b"kira_live_mark_reload\0";
/// Optional, unlike the rest: an older library simply has no accounting.
const HEAP_REPORT: &[u8] = b"kira_rt_heap_report\0";
const STATE_VALUE_INT: &[u8] = b"kira_rt_native_value_int\0";
const STATE_VALUE_ANY: &[u8] = b"kira_rt_native_value_any\0";
const STATE_VALUE_READ_ANY_TYPE: &[u8] = b"kira_rt_native_value_read_any_type\0";
const STATE_VALUE_RAW_PTR: &[u8] = b"kira_rt_native_value_raw_ptr\0";
const CELL_FREE: &[u8] = b"kira_rt_cell_free\0";
const CELL_VM_PROXY_NEW: &[u8] = b"kira_rt_cell_vm_proxy_new\0";
const CELL_VM_PROXY_HANDLE: &[u8] = b"kira_rt_cell_vm_proxy_handle\0";
const STATE_VALUE_CELL: &[u8] = b"kira_rt_native_value_cell\0";
const STATE_VALUE_READ_CELL: &[u8] = b"kira_rt_native_value_read_cell\0";
const STATE_VALUE_FLOAT: &[u8] = b"kira_rt_native_value_float\0";
const STATE_VALUE_BOOL: &[u8] = b"kira_rt_native_value_bool\0";
const STATE_VALUE_STRING: &[u8] = b"kira_rt_native_value_string\0";
const STATE_VALUE_AGGREGATE: &[u8] = b"kira_rt_native_value_aggregate\0";
const STATE_VALUE_SET_CHILD: &[u8] = b"kira_rt_native_value_set_child\0";
const STATE_VALUE_TAG: &[u8] = b"kira_rt_native_value_tag\0";
const STATE_VALUE_READ_INT: &[u8] = b"kira_rt_native_value_read_int\0";
const STATE_VALUE_READ_RAW_PTR: &[u8] = b"kira_rt_native_value_read_raw_ptr\0";
const STATE_VALUE_READ_FLOAT: &[u8] = b"kira_rt_native_value_read_float\0";
const STATE_VALUE_READ_BOOL: &[u8] = b"kira_rt_native_value_read_bool\0";
const STATE_VALUE_READ_STRING: &[u8] = b"kira_rt_native_value_read_string\0";
const STATE_VALUE_LEN: &[u8] = b"kira_rt_native_value_len\0";
const STATE_VALUE_ENUM_TAG: &[u8] = b"kira_rt_native_value_enum_tag\0";
const STATE_VALUE_CHILD: &[u8] = b"kira_rt_native_value_child\0";
const STATE_VALUE_FREE: &[u8] = b"kira_rt_native_value_free\0";
const STATE_NEW: &[u8] = b"kira_rt_native_state_new\0";
const STATE_RECOVER: &[u8] = b"kira_rt_native_state_recover\0";
const STATE_REPLACE: &[u8] = b"kira_rt_native_state_replace\0";
const STATE_FREE: &[u8] = b"kira_rt_native_state_free\0";

/// The native half of a hybrid program, loaded and bound.
pub struct NativeLibrary {
    /// Where the library was loaded from, for diagnostics.
    path: PathBuf,
    /// Each function's trampoline by function id; `None` for a runtime function.
    ///
    /// Indexed by id rather than searched, so reaching a trampoline is a total
    /// function of the id the VM hands over.
    trampolines: Vec<Option<TrampolineFn>>,
    /// Which parameters each function writes through, by function id.
    ///
    /// Read off the manifest at load, beside the trampoline it belongs to,
    /// because the two answer one question together: a trampoline packs a
    /// written-through parameter's final value back into the slot it arrived
    /// in, and this is which slots those are. Every host that calls a
    /// trampoline needs it, so it lives with the trampoline rather than being
    /// looked up again from a manifest each host would have to hold.
    mutable_params: Vec<Vec<bool>>,
    /// The address of each generated callback entry thunk, by callback id.
    callbacks: Vec<u64>,
    str_new: StrNewFn,
    /// Reports the native half.s heap balance; absent in an older library.
    heap_report: Option<HeapReportFn>,
    str_free: StrFreeFn,
    str_data: StrDataFn,
    str_len: StrLenFn,
    install_invoker: InstallInvokerFn,
    live_reload_mark: LiveReloadMarkFn,
    state_value_int: StateIntFn,
    state_value_any: StateAnyFn,
    state_value_read_any_type: StateReadAnyTypeFn,
    state_value_raw_ptr: StateRawPtrFn,
    state_value_cell: StateCellFn,
    state_value_read_cell: StateReadCellFn,
    cell_release: Arc<CellReleaseOwner<libloading::Library>>,
    cell_proxy_new: CellProxyNewFn,
    cell_proxy_handle: CellProxyHandleFn,
    state_value_float: StateFloatFn,
    state_value_bool: StateBoolFn,
    state_value_string: StateStringFn,
    state_value_aggregate: StateAggregateFn,
    state_value_set_child: StateSetChildFn,
    state_value_tag: StateTagFn,
    state_value_read_int: StateReadIntFn,
    state_value_read_raw_ptr: StateReadRawPtrFn,
    state_value_read_float: StateReadFloatFn,
    state_value_read_bool: StateReadBoolFn,
    state_value_read_string: StateReadStringFn,
    state_value_len: StateLenFn,
    state_value_enum_tag: StateEnumTagFn,
    state_value_child: StateChildFn,
    state_value_free: StateNodeFreeFn,
    state_new: StateNewFn,
    state_recover: StateRecoverFn,
    state_replace: StateReplaceFn,
    state_free: StateFreeFn,
    /// The open library. Declared last so it is dropped last: every function
    /// pointer above points into its image and dangles once it is unloaded.
    _library: Arc<libloading::Library>,
}

/// The entry thunk symbol for callback `index`.
///
/// One spelling, imported: `kira_runtime_abi::foreign_callback_name` is the
/// wire contract every side reads — a rename on any one side turning into a
/// missing symbol at load is exactly what a shared function prevents.
fn kira_llvm_backend_callback_name(index: usize) -> String {
    kira_runtime_abi::foreign_callback_name(index)
}

impl NativeLibrary {
    /// Loads `path` and binds every symbol the host needs: the string helpers,
    /// the runtime invoker, one trampoline per native function in `functions`,
    /// and one callback thunk per callback row.
    pub fn load(
        path: &Path,
        functions: &[HybridFunction],
        callbacks: usize,
    ) -> Result<NativeLibrary, HybridError> {
        // Through the shared opener rather than `libloading::Library::new`: the
        // native half may sit beside shared libraries a `runtimeFiles` row put
        // there, and Windows searches the *calling process*'s directory for
        // those unless it is told to search the loaded module's. The library is
        // one this toolchain built and named in a manifest it also wrote; a host
        // that cannot trust its own build has already lost.
        let library = Arc::new(
            kira_dynamic_ffi::open_shared_library(path).map_err(|source| HybridError::Library {
                path: path.to_path_buf(),
                source,
            })?,
        );

        let str_new = bind(&library, path, STR_NEW)?;
        let str_free = bind(&library, path, STR_FREE)?;
        let str_data = bind(&library, path, STR_DATA)?;
        let str_len = bind(&library, path, STR_LEN)?;
        let install_invoker = bind(&library, path, INSTALL_INVOKER)?;
        let live_reload_mark = bind(&library, path, LIVE_RELOAD_MARK)?;
        // Optional: a library built before heap accounting existed simply has
        // no such symbol, and that is not a reason to refuse to load it.
        let heap_report: Option<HeapReportFn> = bind(&library, path, HEAP_REPORT).ok();
        let state_value_int = bind(&library, path, STATE_VALUE_INT)?;
        let state_value_any = bind(&library, path, STATE_VALUE_ANY)?;
        let state_value_read_any_type = bind(&library, path, STATE_VALUE_READ_ANY_TYPE)?;
        let state_value_raw_ptr = bind(&library, path, STATE_VALUE_RAW_PTR)?;
        let state_value_cell = bind(&library, path, STATE_VALUE_CELL)?;
        let state_value_read_cell = bind(&library, path, STATE_VALUE_READ_CELL)?;
        let cell_free = bind(&library, path, CELL_FREE)?;
        let cell_release = Arc::new(CellReleaseOwner::new(Arc::clone(&library), cell_free));
        let cell_proxy_new = bind(&library, path, CELL_VM_PROXY_NEW)?;
        let cell_proxy_handle = bind(&library, path, CELL_VM_PROXY_HANDLE)?;
        let state_value_float = bind(&library, path, STATE_VALUE_FLOAT)?;
        let state_value_bool = bind(&library, path, STATE_VALUE_BOOL)?;
        let state_value_string = bind(&library, path, STATE_VALUE_STRING)?;
        let state_value_aggregate = bind(&library, path, STATE_VALUE_AGGREGATE)?;
        let state_value_set_child = bind(&library, path, STATE_VALUE_SET_CHILD)?;
        let state_value_tag = bind(&library, path, STATE_VALUE_TAG)?;
        let state_value_read_int = bind(&library, path, STATE_VALUE_READ_INT)?;
        let state_value_read_raw_ptr = bind(&library, path, STATE_VALUE_READ_RAW_PTR)?;
        let state_value_read_float = bind(&library, path, STATE_VALUE_READ_FLOAT)?;
        let state_value_read_bool = bind(&library, path, STATE_VALUE_READ_BOOL)?;
        let state_value_read_string = bind(&library, path, STATE_VALUE_READ_STRING)?;
        let state_value_len = bind(&library, path, STATE_VALUE_LEN)?;
        let state_value_enum_tag = bind(&library, path, STATE_VALUE_ENUM_TAG)?;
        let state_value_child = bind(&library, path, STATE_VALUE_CHILD)?;
        let state_value_free = bind(&library, path, STATE_VALUE_FREE)?;
        let state_new = bind(&library, path, STATE_NEW)?;
        let state_recover = bind(&library, path, STATE_RECOVER)?;
        let state_replace = bind(&library, path, STATE_REPLACE)?;
        let state_free = bind(&library, path, STATE_FREE)?;

        let mut trampolines = vec![None; functions.len()];
        let mut mutable_params = vec![Vec::new(); functions.len()];
        for function in functions {
            if let Some(slot) = mutable_params.get_mut(function.id as usize) {
                *slot = function
                    .params
                    .iter()
                    .map(|param| param.ownership.is_mutable())
                    .collect();
            }
            let Some(symbol) = &function.exported_name else {
                continue;
            };
            let mut name = symbol.clone().into_bytes();
            name.push(0);
            let trampoline: TrampolineFn = bind(&library, path, &name)?;
            let slot = trampolines.get_mut(function.id as usize).ok_or_else(|| {
                HybridError::Mismatch(format!(
                    "function `{}` has id {} but the manifest carries only {} functions",
                    function.name,
                    function.id,
                    functions.len(),
                ))
            })?;
            *slot = Some(trampoline);
        }

        // One entry thunk per callback row, bound by the name the backend gave
        // it. Their addresses are what a `@FFI.Callback` value carries; nothing
        // here ever calls one, because C is what does.
        let mut callback_entries = Vec::with_capacity(callbacks);
        for index in 0..callbacks {
            let mut name = kira_llvm_backend_callback_name(index).into_bytes();
            name.push(0);
            let entry: unsafe extern "C" fn() = bind(&library, path, &name)?;
            callback_entries.push(entry as usize as u64);
        }

        Ok(NativeLibrary {
            path: path.to_path_buf(),
            trampolines,
            mutable_params,
            callbacks: callback_entries,
            str_new,
            heap_report,
            str_free,
            str_data,
            str_len,
            install_invoker,
            live_reload_mark,
            state_value_int,
            state_value_any,
            state_value_read_any_type,
            state_value_raw_ptr,
            state_value_cell,
            state_value_read_cell,
            cell_release,
            cell_proxy_new,
            cell_proxy_handle,
            state_value_float,
            state_value_bool,
            state_value_string,
            state_value_aggregate,
            state_value_set_child,
            state_value_tag,
            state_value_read_int,
            state_value_read_raw_ptr,
            state_value_read_float,
            state_value_read_bool,
            state_value_read_string,
            state_value_len,
            state_value_enum_tag,
            state_value_child,
            state_value_free,
            state_new,
            state_recover,
            state_replace,
            state_free,
            _library: library,
        })
    }

    /// Asks the native half to report its heap balance, if it can.
    ///
    /// A hybrid program's native half is a shared library with no `main`, so
    /// nothing in it runs at exit — the host has to ask. Silent unless
    /// `KIRA_HEAP_REPORT` is set, and a no-op for a library built before
    /// accounting existed.
    pub fn report_heap(&self) {
        let Some(report) = self.heap_report else {
            return;
        };
        // SAFETY: the symbol was bound from this library, which is still
        // loaded, and it takes and returns nothing.
        unsafe { report() };
    }

    /// Marks the next graphics callback as a VM reload boundary.
    pub fn mark_live_reload(&self) {
        // SAFETY: the symbol was bound from this library, which remains loaded
        // for as long as `self` and has no arguments or return value.
        unsafe { (self.live_reload_mark)() };
    }

    /// Where this library was loaded from.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The trampoline for `function_id`, or `None` when it names no native
    /// function.
    pub fn trampoline(&self, function_id: u32) -> Option<TrampolineFn> {
        self.trampolines
            .get(function_id as usize)
            .copied()
            .flatten()
    }

    /// Which of `function_id`'s parameters the callee writes through.
    ///
    /// Empty for a function the manifest has no row for — which a call cannot
    /// reach, because [`NativeLibrary::trampoline`] answers `None` first.
    pub fn mutable_params(&self, function_id: u32) -> &[bool] {
        self.mutable_params
            .get(function_id as usize)
            .map(Vec::as_slice)
            .unwrap_or_default()
    }

    /// The address C enters Kira at for callback `callback_id`, or `None` when
    /// the id names no callback this library bound.
    pub fn callback_address(&self, callback_id: u32) -> Option<u64> {
        self.callbacks.get(callback_id as usize).copied()
    }

    /// Installs the host's runtime invoker, or clears it with `None`.
    ///
    /// # Safety
    /// `invoker` must stay callable until it is cleared or the process exits.
    pub unsafe fn install_invoker(&self, invoker: Option<RuntimeInvoker>) {
        // SAFETY: forwarding the caller's own guarantee to the library's
        // installer, whose contract is the same one.
        unsafe { (self.install_invoker)(invoker) };
    }

    /// Calls `trampoline` with `args`, returning what it wrote.
    ///
    /// # Safety
    /// `trampoline` must be one of this library's, and `args` must match the
    /// signature the manifest records for it — the callee is machine code that
    /// reads them by position and cannot check.
    pub unsafe fn call(&self, trampoline: TrampolineFn, args: &mut [BridgeValue]) -> BridgeValue {
        let mut out = BridgeValue::VOID;
        let count = args.len() as u32;
        // A zero-length `Vec`'s pointer is dangling-but-aligned rather than
        // null; the ABI permits null when the count is 0, so say null.
        let pointer = if args.is_empty() {
            std::ptr::null_mut()
        } else {
            args.as_mut_ptr()
        };
        // SAFETY: `pointer` covers `count` readable values (or is null when
        // there are none) and `out` is one writable value on this stack frame.
        // The caller vouches for the signature match.
        unsafe { trampoline(pointer, count, &mut out) };
        out
    }

    /// Copies `text` into a fresh handle from the library's own allocator.
    pub fn new_string(&self, text: &str) -> StrHandle {
        // SAFETY: the slice covers exactly `len` readable bytes.
        let handle = unsafe { (self.str_new)(text.as_ptr(), text.len()) };
        handle as StrHandle
    }

    /// Copies a handle's bytes out, leaving the handle live.
    ///
    /// # Safety
    /// `handle` must be null or a live handle from this library.
    pub unsafe fn read_string(&self, handle: StrHandle) -> Result<String, std::str::Utf8Error> {
        let pointer = handle as *mut c_void;
        // SAFETY: the caller vouches the handle is live (or null); both helpers
        // accept null as the empty string.
        let (data, len) = unsafe { ((self.str_data)(pointer), (self.str_len)(pointer)) };
        if len == 0 {
            return Ok(String::new());
        }
        // SAFETY: `kira_rt_str_data` returns a pointer valid for
        // `kira_rt_str_len` bytes for as long as the handle is unfreed, and
        // nothing frees it between those two calls and this read.
        let bytes = unsafe { std::slice::from_raw_parts(data, len) };
        // Validated rather than trusted: the bytes crossed an ABI, and a wrong
        // answer is worse than a rejection.
        std::str::from_utf8(bytes).map(str::to_owned)
    }

    /// Frees a string handle. A null handle is a no-op.
    ///
    /// # Safety
    /// `handle` must be null or a live handle from this library, freed once.
    pub unsafe fn free_string(&self, handle: StrHandle) {
        // SAFETY: the caller upholds the free-once contract.
        unsafe { (self.str_free)(handle as *mut c_void) };
    }

    /// Frees one callback-state node this library allocated.
    ///
    /// Exists for a marshaler that created several transfers and then failed
    /// partway: everything already created is still this library's to release,
    /// and the callee that would have consumed them never runs.
    ///
    /// # Safety
    /// `node` must be null or a live node from this library, freed once.
    pub unsafe fn free_state_node(&self, node: *mut c_void) {
        // SAFETY: the caller upholds the free-once contract.
        unsafe { (self.state_value_free)(node as StateNode) };
    }

    /// Copies a handle's bytes out and frees it: the host taking ownership.
    ///
    /// # Safety
    /// `handle` must be null or a live handle from this library, freed once —
    /// which this does.
    pub unsafe fn take_string(&self, handle: StrHandle) -> Result<String, std::str::Utf8Error> {
        // SAFETY: the caller vouches the handle is live; read before free.
        let text = unsafe { self.read_string(handle) };
        // SAFETY: the same handle, consumed exactly once here, and not read
        // again — the bytes were copied out above, including on the error path.
        unsafe { self.free_string(handle) };
        text
    }

    /// Boxes callback state in the loaded native half's process-lifetime store.
    pub fn native_state_create(
        &self,
        ty: NativeStateTypeId,
        value: NativeStateValue,
    ) -> Result<NativeStateToken, NativeStateError> {
        // SAFETY: every node is allocated and consumed by this same loaded library.
        let node = unsafe { self.encode_state_value(&value)? };
        let mut token = 0;
        // SAFETY: `node` is live and `token` is one writable word.
        let status = unsafe { (self.state_new)(ty.as_word(), node, &mut token) };
        self.check_state_status(status, token)?;
        Ok(NativeStateToken::from_word(token))
    }

    /// Recovers an owned callback-state copy from the loaded native half.
    pub fn native_state_recover(
        &self,
        token: NativeStateToken,
        ty: NativeStateTypeId,
    ) -> Result<NativeStateValue, NativeStateError> {
        let mut node = std::ptr::null_mut();
        // SAFETY: `node` is one writable pointer slot.
        let status = unsafe { (self.state_recover)(token.as_word(), ty.as_word(), &mut node) };
        self.check_state_status(status, token.as_word())?;
        // SAFETY: success initializes one live node from this library.
        unsafe { self.decode_state_value(node) }
    }

    /// Replaces callback state in the loaded native half.
    pub fn native_state_replace(
        &self,
        token: NativeStateToken,
        ty: NativeStateTypeId,
        value: NativeStateValue,
    ) -> Result<(), NativeStateError> {
        // SAFETY: every node is allocated and consumed by this same loaded library.
        let node = unsafe { self.encode_state_value(&value)? };
        // SAFETY: `node` is live and consumed by the runtime call.
        let status = unsafe { (self.state_replace)(token.as_word(), ty.as_word(), node) };
        self.check_state_status(status, token.as_word())
    }

    /// Releases callback state in the loaded native half exactly once.
    pub fn native_state_free(&self, token: NativeStateToken) -> Result<(), NativeStateError> {
        // SAFETY: this function pointer accepts any token word and validates it.
        let status = unsafe { (self.state_free)(token.as_word()) };
        self.check_state_status(status, token.as_word())
    }

    /// Builds the library's node tree from a stored value, reading it.
    ///
    /// Read rather than consumed: an aggregate's children are shared, so taking
    /// ownership of one would mean copying the whole subtree just to walk it.
    pub(crate) unsafe fn encode_state_value(
        &self,
        value: &NativeStateValue,
    ) -> Result<StateNode, NativeStateError> {
        Ok(match value {
            // SAFETY: the constructor accepts its scalar by value.
            NativeStateValue::Int(value) => unsafe { (self.state_value_int)(*value) },
            NativeStateValue::Any { type_id, payload } => {
                // SAFETY: the child is allocated by this library and ownership
                // moves into the parent node.
                let child = unsafe { self.encode_state_value(payload)? };
                // SAFETY: the constructor consumes the live child exactly once.
                unsafe { (self.state_value_any)(*type_id, child) }
            }
            // SAFETY: the constructor accepts its opaque word by value.
            NativeStateValue::RawPtr(value) => unsafe { (self.state_value_raw_ptr)(*value) },
            // SAFETY: the constructor accepts its scalar by value.
            NativeStateValue::Float(value) => unsafe { (self.state_value_float)(*value) },
            // SAFETY: the constructor accepts its scalar by value.
            NativeStateValue::Bool(value) => unsafe { (self.state_value_bool)(u8::from(*value)) },
            NativeStateValue::String(value) => {
                let string = self.new_string(value) as *mut c_void;
                // SAFETY: the constructor consumes this live string handle.
                unsafe { (self.state_value_string)(string) }
            }
            // SAFETY: the aggregate owns every child this builds.
            NativeStateValue::Struct(values) => unsafe {
                self.encode_aggregate(NativeStateValueTag::STRUCT, 0, values)?
            },
            // SAFETY: the aggregate owns every child this builds.
            NativeStateValue::Array(values) => unsafe {
                self.encode_aggregate(NativeStateValueTag::ARRAY, 0, values)?
            },
            NativeStateValue::Enum { tag, payload } => {
                let values = payload.as_deref().map_or(&[][..], std::slice::from_ref);
                // SAFETY: the aggregate owns every child this builds.
                unsafe { self.encode_aggregate(NativeStateValueTag::ENUM, *tag, values)? }
            }
            // A cell crosses as an opaque handle; the declaring half owns its
            // storage.
            NativeStateValue::Cell(cell) => {
                if cell.is_vm_owned() {
                    // SAFETY: the proxy is a fresh native cell box. The state
                    // node takes one share, and this releases the constructor
                    // share immediately below.
                    let proxy = unsafe { (self.cell_proxy_new)(cell.handle()) };
                    // SAFETY: `proxy` is that fresh box, so the node takes its
                    // own share of a live cell.
                    let node = unsafe { (self.state_value_cell)(proxy) };
                    // SAFETY: this releases the constructor share only; the
                    // node above holds the one that keeps the box alive.
                    unsafe { (self.cell_release.free)(proxy) };
                    node
                } else {
                    // SAFETY: the constructor takes the box by handle and
                    // takes its own share; this node keeps the one it had.
                    unsafe { (self.state_value_cell)(cell.handle()) }
                }
            }
        })
    }

    unsafe fn encode_aggregate(
        &self,
        tag: NativeStateValueTag,
        enum_tag: u32,
        values: &[NativeStateValue],
    ) -> Result<StateNode, NativeStateError> {
        // SAFETY: constructor takes plain scalar metadata.
        let node = unsafe { (self.state_value_aggregate)(tag.0, enum_tag, values.len()) };
        for (index, value) in values.iter().enumerate() {
            // SAFETY: recursion allocates a child in this same library.
            let child = unsafe { self.encode_state_value(value)? };
            // SAFETY: node and child are live; each in-range slot is set once.
            let status = unsafe { (self.state_value_set_child)(node, index, child) };
            if status != NativeStateStatus::OK.0 {
                // SAFETY: the parent remains live after a refused child store.
                unsafe { (self.state_value_free)(node) };
                return Err(NativeStateError::MalformedValue);
            }
        }
        Ok(node)
    }

    pub(crate) unsafe fn decode_state_value(
        &self,
        node: StateNode,
    ) -> Result<NativeStateValue, NativeStateError> {
        if node.is_null() {
            return Err(NativeStateError::MalformedValue);
        }
        // SAFETY: `node` is live and belongs to this library.
        let tag = NativeStateValueTag(unsafe { (self.state_value_tag)(node) });
        let value = match tag {
            NativeStateValueTag::INT => {
                // SAFETY: tag validation established the node shape.
                NativeStateValue::Int(unsafe { (self.state_value_read_int)(node) })
            }
            NativeStateValueTag::ANY => {
                // SAFETY: tag validation established the node shape.
                let type_id = unsafe { (self.state_value_read_any_type)(node) };
                // SAFETY: the `Any` node has exactly one owned payload child.
                let child = unsafe { (self.state_value_child)(node, 0) };
                // SAFETY: recursion consumes the child. A payload that fails
                // to decode frees this node first: an early return here would
                // otherwise strand it, with every not-yet-decoded sibling
                // behind it.
                let payload = match unsafe { self.decode_state_value(child) } {
                    Ok(payload) => payload,
                    Err(error) => {
                        // SAFETY: `node` is still live and uniquely owned.
                        unsafe { (self.state_value_free)(node) };
                        return Err(error);
                    }
                };
                NativeStateValue::any_of(type_id, payload)
            }
            NativeStateValueTag::RAW_PTR => {
                // SAFETY: tag validation established the node shape.
                NativeStateValue::RawPtr(unsafe { (self.state_value_read_raw_ptr)(node) })
            }
            NativeStateValueTag::CELL => {
                // SAFETY: tag validation established the node shape.
                let handle = unsafe { (self.state_value_read_cell)(node) };
                // Keep the loaded library with the share's release function.
                let release = Arc::clone(&self.cell_release);
                NativeStateValue::Cell(kira_runtime_abi::NativeCell::new(handle, move |handle| {
                    release.release(handle);
                }))
            }
            NativeStateValueTag::FLOAT => {
                // SAFETY: tag validation established the node shape.
                NativeStateValue::Float(unsafe { (self.state_value_read_float)(node) })
            }
            NativeStateValueTag::BOOL => {
                // SAFETY: tag validation established the node shape.
                NativeStateValue::Bool(unsafe { (self.state_value_read_bool)(node) } != 0)
            }
            NativeStateValueTag::STRING => {
                // SAFETY: tag validation established the node shape.
                let handle = unsafe { (self.state_value_read_string)(node) } as StrHandle;
                // SAFETY: the reader returned one owned handle from this library.
                let text = unsafe { self.take_string(handle) }
                    .map_err(|_| NativeStateError::MalformedValue)?;
                NativeStateValue::String(text)
            }
            NativeStateValueTag::STRUCT | NativeStateValueTag::ARRAY => {
                // SAFETY: aggregate accessors accept this live node.
                let len = unsafe { (self.state_value_len)(node) };
                let mut values = Vec::with_capacity(len);
                for index in 0..len {
                    // SAFETY: `index < len`; the returned child is owned.
                    let child = unsafe { (self.state_value_child)(node, index) };
                    // SAFETY: recursion consumes that owned child. A child
                    // that fails frees this node before the error returns —
                    // the values decoded so far are dropped with `values`,
                    // and this node would otherwise be skipped by the free
                    // at the function's tail.
                    let value = match unsafe { self.decode_state_value(child) } {
                        Ok(value) => value,
                        Err(error) => {
                            // SAFETY: `node` is still live and uniquely owned.
                            unsafe { (self.state_value_free)(node) };
                            return Err(error);
                        }
                    };
                    values.push(value);
                }
                if tag == NativeStateValueTag::STRUCT {
                    NativeStateValue::struct_of(values)
                } else {
                    NativeStateValue::array_of(values)
                }
            }
            NativeStateValueTag::ENUM => {
                // SAFETY: enum accessors accept this live node.
                let enum_tag = unsafe { (self.state_value_enum_tag)(node) };
                // SAFETY: same live aggregate node.
                let len = unsafe { (self.state_value_len)(node) };
                let payload = if len == 0 {
                    None
                } else if len == 1 {
                    // SAFETY: child zero exists and is returned owned.
                    let child = unsafe { (self.state_value_child)(node, 0) };
                    // SAFETY: recursion consumes the child. A payload that
                    // fails frees this node first, as every other error path
                    // here does.
                    let decoded = unsafe { self.decode_state_value(child) };
                    match decoded {
                        Ok(payload) => Some(payload),
                        Err(error) => {
                            // SAFETY: `node` is still live and uniquely owned.
                            unsafe { (self.state_value_free)(node) };
                            return Err(error);
                        }
                    }
                } else {
                    // SAFETY: `node` is still live and uniquely owned.
                    unsafe { (self.state_value_free)(node) };
                    return Err(NativeStateError::MalformedValue);
                };
                NativeStateValue::enum_of(enum_tag, payload)
            }
            _ => {
                // SAFETY: `node` is still live and uniquely owned.
                unsafe { (self.state_value_free)(node) };
                return Err(NativeStateError::MalformedValue);
            }
        };
        // SAFETY: decoding copied or cloned every value out; release the node.
        unsafe { (self.state_value_free)(node) };
        Ok(value)
    }

    /// Returns the VM word carried by a native callback-state cell proxy.
    pub(crate) fn vm_cell_proxy_handle(&self, handle: u64) -> Option<u64> {
        // SAFETY: the loaded library validates null, inline, and ordinary
        // handles before reading the proxy tag.
        let value = unsafe { (self.cell_proxy_handle)(handle) };
        (value != u64::MAX).then_some(value)
    }

    fn check_state_status(&self, status: u32, token: u64) -> Result<(), NativeStateError> {
        match NativeStateStatus(status) {
            NativeStateStatus::OK => Ok(()),
            NativeStateStatus::NO_HOST => Err(NativeStateError::NoStateHost),
            NativeStateStatus::NULL_TOKEN => Err(NativeStateError::NullToken),
            NativeStateStatus::UNKNOWN_TOKEN => Err(NativeStateError::UnknownToken(token)),
            NativeStateStatus::WRONG_TYPE => Err(NativeStateError::WrongType {
                actual: 0,
                requested: 0,
            }),
            NativeStateStatus::TOKEN_EXHAUSTED => Err(NativeStateError::TokenExhausted),
            _ => Err(NativeStateError::MalformedValue),
        }
    }
}

/// Resolves one symbol out of `library`, naming it when it is absent.
fn bind<T: Copy>(
    library: &libloading::Library,
    path: &Path,
    symbol: &[u8],
) -> Result<T, HybridError> {
    // SAFETY: `T` is the signature this crate declares for `symbol`, and the
    // library is one this toolchain built against the same declarations. The
    // returned value is copied out of the `Symbol` and stays valid while the
    // library is loaded, which `NativeLibrary` guarantees by owning it.
    let resolved =
        unsafe { library.get::<T>(symbol) }.map_err(|source| HybridError::MissingSymbol {
            path: path.to_path_buf(),
            symbol: String::from_utf8_lossy(symbol.strip_suffix(b"\0").unwrap_or(symbol))
                .into_owned(),
            source,
        })?;
    Ok(*resolved)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    struct DropProbe(Arc<AtomicUsize>);

    impl Drop for DropProbe {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }

    unsafe extern "C" fn test_cell_free(_handle: u64) {}

    #[test]
    fn a_cell_release_keeps_its_library_lease_alive() {
        let drops = Arc::new(AtomicUsize::new(0));
        let library = Arc::new(DropProbe(Arc::clone(&drops)));
        let owner = Arc::new(CellReleaseOwner::new(Arc::clone(&library), test_cell_free));
        drop(library);

        let release = Arc::clone(&owner);
        let cell = kira_runtime_abi::NativeCell::new(7, move |handle| {
            release.release(handle);
        });
        drop(owner);
        assert_eq!(drops.load(Ordering::Relaxed), 0);

        drop(cell);
        assert_eq!(drops.load(Ordering::Relaxed), 1);
    }
}
