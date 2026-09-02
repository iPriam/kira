//! The loaded native half: `dlopen`, symbol binding, and the string helpers the
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
    BridgeValue, CBlockOffset, ForeignPointerWidth, NativeCBlock, NativeStateError,
    NativeStateStatus, NativeStateToken, NativeStateTypeId, NativeStateValue, NativeStateValueTag,
};

use crate::error::HybridError;

mod native_state;

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
type TaskResetFn = unsafe extern "C" fn();
type MainThreadRunFn = unsafe extern "C" fn(extern "C" fn() -> i32) -> i32;
type MainThreadInstallDispatcherFn = unsafe extern "C" fn(*mut c_void);
type MainThreadDispatcherFn = unsafe extern "C" fn(u32, *mut BridgeValue, u32, *mut BridgeValue);
type MainThreadLifecycleResolverFn = unsafe extern "C" fn(u32) -> *mut c_void;
type MainThreadLifecycleStartFn = unsafe extern "C" fn(u32) -> u8;
type MainThreadLifecyclePumpFn = unsafe extern "C" fn(u64) -> u8;
type MainThreadLifecycleResetFn = unsafe extern "C" fn();
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
type StateCBlockFn = unsafe extern "C" fn(*const u8, usize, usize) -> StateNode;
type StateSetCBlockChildFn = unsafe extern "C" fn(StateNode, usize, u64, u32, StateNode) -> u32;
type StateReadCBlockLenFn = unsafe extern "C" fn(StateNode) -> usize;
type StateReadCBlockDataFn = unsafe extern "C" fn(StateNode) -> *const u8;
type StateReadCBlockChildOffsetFn = unsafe extern "C" fn(StateNode, usize) -> u64;
type StateReadCBlockChildWidthFn = unsafe extern "C" fn(StateNode, usize) -> u32;
type StateEnumTagFn = unsafe extern "C" fn(StateNode) -> u32;
type StateChildFn = unsafe extern "C" fn(StateNode, usize) -> StateNode;
type StateNodeFreeFn = unsafe extern "C" fn(StateNode);
type StateNewFn = unsafe extern "C" fn(u64, StateNode, *mut u64) -> u32;
type StateRecoverFn = unsafe extern "C" fn(u64, u64, *mut StateNode) -> u32;
type StateReplaceFn = unsafe extern "C" fn(u64, u64, StateNode) -> u32;
type StateCountFn = unsafe extern "C" fn(u64) -> u32;
type CBlockReleaseRetainedFn = unsafe extern "C" fn();
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
const TASK_RESET: &[u8] = b"kira_rt_task_reset\0";
const MAIN_THREAD_RUN: &[u8] = b"kira_rt_main_thread_run\0";
const MAIN_THREAD_INSTALL_DISPATCHER: &[u8] = b"kira_rt_main_thread_install_dispatcher\0";
const MAIN_THREAD_DISPATCHER: &[u8] = b"kira_main_thread_dispatch\0";
const MAIN_THREAD_INSTALL_LIFECYCLE_RESOLVER: &[u8] =
    b"kira_rt_main_thread_install_lifecycle_resolver\0";
const MAIN_THREAD_LIFECYCLE_RESOLVER: &[u8] = b"kira_main_thread_lifecycle_resolve\0";
const MAIN_THREAD_LIFECYCLE_START: &[u8] = b"kira_rt_main_thread_lifecycle_start_local\0";
const MAIN_THREAD_LIFECYCLE_PUMP: &[u8] = b"kira_rt_main_thread_lifecycle_pump_local\0";
const MAIN_THREAD_LIFECYCLE_RESET: &[u8] = b"kira_rt_main_thread_lifecycle_reset_local\0";
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
const STATE_VALUE_CBLOCK: &[u8] = b"kira_rt_native_value_cblock\0";
const STATE_VALUE_SET_CBLOCK_CHILD: &[u8] = b"kira_rt_native_value_set_cblock_child\0";
const STATE_VALUE_READ_CBLOCK_LEN: &[u8] = b"kira_rt_native_value_read_cblock_len\0";
const STATE_VALUE_READ_CBLOCK_DATA: &[u8] = b"kira_rt_native_value_read_cblock_data\0";
const STATE_VALUE_CBLOCK_CHILD_OFFSET: &[u8] = b"kira_rt_native_value_cblock_child_offset\0";
const STATE_VALUE_CBLOCK_CHILD_WIDTH: &[u8] = b"kira_rt_native_value_cblock_child_width\0";
const STATE_VALUE_ENUM_TAG: &[u8] = b"kira_rt_native_value_enum_tag\0";
const STATE_VALUE_CHILD: &[u8] = b"kira_rt_native_value_child\0";
const STATE_VALUE_FREE: &[u8] = b"kira_rt_native_value_free\0";
const STATE_NEW: &[u8] = b"kira_rt_native_state_new\0";
const STATE_RECOVER: &[u8] = b"kira_rt_native_state_recover\0";
const STATE_REPLACE: &[u8] = b"kira_rt_native_state_replace\0";
const STATE_RETAIN: &[u8] = b"kira_rt_native_state_retain\0";
const STATE_RELEASE: &[u8] = b"kira_rt_native_state_release\0";
const CBLOCK_RELEASE_RETAINED: &[u8] = b"kira_rt_cblock_release_retained\0";

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
    /// Starts and ends the native task table's per-run scope.
    task_reset: TaskResetFn,
    main_thread_run: MainThreadRunFn,
    main_thread_install_dispatcher: MainThreadInstallDispatcherFn,
    main_thread_dispatcher: Option<MainThreadDispatcherFn>,
    main_thread_install_lifecycle_resolver: MainThreadInstallDispatcherFn,
    main_thread_lifecycle_resolver: MainThreadLifecycleResolverFn,
    main_thread_lifecycle_start: MainThreadLifecycleStartFn,
    main_thread_lifecycle_pump: MainThreadLifecyclePumpFn,
    main_thread_lifecycle_reset: MainThreadLifecycleResetFn,
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
    state_value_cblock: StateCBlockFn,
    state_value_set_cblock_child: StateSetCBlockChildFn,
    state_value_read_cblock_len: StateReadCBlockLenFn,
    state_value_read_cblock_data: StateReadCBlockDataFn,
    state_value_cblock_child_offset: StateReadCBlockChildOffsetFn,
    state_value_cblock_child_width: StateReadCBlockChildWidthFn,
    state_value_enum_tag: StateEnumTagFn,
    state_value_child: StateChildFn,
    state_value_free: StateNodeFreeFn,
    state_new: StateNewFn,
    state_recover: StateRecoverFn,
    state_replace: StateReplaceFn,
    state_retain: StateCountFn,
    state_release: StateCountFn,
    cblock_release_retained: CBlockReleaseRetainedFn,
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
    /// Loads `path` — or, for [`None`], binds this process image itself — and
    /// binds every symbol the host needs: the string helpers, the runtime
    /// invoker, one trampoline per native function in `functions`, and one
    /// callback thunk per callback row.
    ///
    /// `None` is the embedded-application layout: the native half was linked
    /// into the running binary and its manifest recorded
    /// [`SELF_LIBRARY_MARKER`][kira_dynamic_ffi::SELF_LIBRARY_MARKER] rather
    /// than a file name. Every symbol must therefore be exported by the image,
    /// which is what an embedded link's `-Wl,-export_dynamic` is for.
    pub fn load(
        path: Option<&Path>,
        functions: &[HybridFunction],
        callbacks: usize,
    ) -> Result<NativeLibrary, HybridError> {
        let (display_path, library) = match path {
            Some(path) => {
                // Through the shared opener rather than `libloading::Library::new`: the
                // native half may sit beside shared libraries a `runtimeFiles` row put
                // there, and Windows searches the *calling process*'s directory for
                // those unless it is told to search the loaded module's. The library is
                // one this toolchain built and named in a manifest it also wrote; a host
                // that cannot trust its own build has already lost.
                let library = Arc::new(kira_dynamic_ffi::open_shared_library(path).map_err(
                    |source| HybridError::Library {
                        path: path.to_path_buf(),
                        source,
                    },
                )?);
                (path.display().to_string(), library)
            }
            None => (
                kira_dynamic_ffi::SELF_LIBRARY_MARKER.to_owned(),
                Arc::new(kira_dynamic_ffi::open_process_image().map_err(HybridError::SelfLibrary)?),
            ),
        };
        let path = PathBuf::from(display_path);
        let path = path.as_path();

        let str_new = bind(&library, path, STR_NEW)?;
        let str_free = bind(&library, path, STR_FREE)?;
        let str_data = bind(&library, path, STR_DATA)?;
        let str_len = bind(&library, path, STR_LEN)?;
        let install_invoker = bind(&library, path, INSTALL_INVOKER)?;
        let live_reload_mark = bind(&library, path, LIVE_RELOAD_MARK)?;
        // Optional: a library built before heap accounting existed simply has
        // no such symbol, and that is not a reason to refuse to load it.
        let heap_report: Option<HeapReportFn> = bind(&library, path, HEAP_REPORT).ok();
        let task_reset = bind(&library, path, TASK_RESET)?;
        let main_thread_run = bind(&library, path, MAIN_THREAD_RUN)?;
        let main_thread_install_dispatcher = bind(&library, path, MAIN_THREAD_INSTALL_DISPATCHER)?;
        let main_thread_dispatcher = bind(&library, path, MAIN_THREAD_DISPATCHER).ok();
        let main_thread_install_lifecycle_resolver =
            bind(&library, path, MAIN_THREAD_INSTALL_LIFECYCLE_RESOLVER)?;
        let main_thread_lifecycle_resolver = bind(&library, path, MAIN_THREAD_LIFECYCLE_RESOLVER)?;
        let main_thread_lifecycle_start = bind(&library, path, MAIN_THREAD_LIFECYCLE_START)?;
        let main_thread_lifecycle_pump = bind(&library, path, MAIN_THREAD_LIFECYCLE_PUMP)?;
        let main_thread_lifecycle_reset = bind(&library, path, MAIN_THREAD_LIFECYCLE_RESET)?;
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
        let state_value_cblock = bind(&library, path, STATE_VALUE_CBLOCK)?;
        let state_value_set_cblock_child = bind(&library, path, STATE_VALUE_SET_CBLOCK_CHILD)?;
        let state_value_read_cblock_len = bind(&library, path, STATE_VALUE_READ_CBLOCK_LEN)?;
        let state_value_read_cblock_data = bind(&library, path, STATE_VALUE_READ_CBLOCK_DATA)?;
        let state_value_cblock_child_offset =
            bind(&library, path, STATE_VALUE_CBLOCK_CHILD_OFFSET)?;
        let state_value_cblock_child_width = bind(&library, path, STATE_VALUE_CBLOCK_CHILD_WIDTH)?;
        let state_value_enum_tag = bind(&library, path, STATE_VALUE_ENUM_TAG)?;
        let state_value_child = bind(&library, path, STATE_VALUE_CHILD)?;
        let state_value_free = bind(&library, path, STATE_VALUE_FREE)?;
        let state_new = bind(&library, path, STATE_NEW)?;
        let state_recover = bind(&library, path, STATE_RECOVER)?;
        let state_replace = bind(&library, path, STATE_REPLACE)?;
        let state_retain = bind(&library, path, STATE_RETAIN)?;
        let state_release = bind(&library, path, STATE_RELEASE)?;
        let cblock_release_retained = bind(&library, path, CBLOCK_RELEASE_RETAINED)?;

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
            task_reset,
            main_thread_run,
            main_thread_install_dispatcher,
            main_thread_dispatcher,
            main_thread_install_lifecycle_resolver,
            main_thread_lifecycle_resolver,
            main_thread_lifecycle_start,
            main_thread_lifecycle_pump,
            main_thread_lifecycle_reset,
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
            state_value_cblock,
            state_value_set_cblock_child,
            state_value_read_cblock_len,
            state_value_read_cblock_data,
            state_value_cblock_child_offset,
            state_value_cblock_child_width,
            state_value_enum_tag,
            state_value_child,
            state_value_free,
            state_new,
            state_recover,
            state_replace,
            state_retain,
            state_release,
            cblock_release_retained,
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

    /// Starts or ends the task scope associated with the current host thread.
    pub fn reset_tasks(&self) {
        // SAFETY: the symbol was bound from this library, which remains loaded
        // for as long as `self` and has no arguments or return value.
        unsafe { (self.task_reset)() };
    }

    /// Installs the generated main-thread dispatcher for this native image.
    ///
    /// A hybrid image may not contain one when no `@MainThread` target is
    /// reachable; in that case the runtime is cleared rather than handed a
    /// stale function pointer from a previous session.
    pub fn install_main_thread_dispatcher(&self) {
        let pointer = self
            .main_thread_dispatcher
            .map_or(std::ptr::null_mut(), |dispatcher| dispatcher as *mut c_void);
        // SAFETY: the installer was resolved from this still-loaded image, and
        // the optional dispatcher is either from that image or null.
        unsafe { (self.main_thread_install_dispatcher)(pointer) };
        // SAFETY: both functions were resolved from this same still-loaded
        // image, and the resolver remains valid throughout the run.
        unsafe {
            (self.main_thread_install_lifecycle_resolver)(
                self.main_thread_lifecycle_resolver as *mut c_void,
            )
        };
    }

    /// Runs a native entry through the image's helper-thread/main-thread loop.
    pub fn run_main_thread(&self, entry: extern "C" fn() -> i32) -> i32 {
        // SAFETY: `entry` is a generated C-ABI helper that remains valid for
        // the duration of this call, and the runtime owns the helper thread.
        unsafe { (self.main_thread_run)(entry) }
    }

    /// Starts one native lifecycle in a host-owned main-thread loop.
    pub fn start_main_thread_lifecycle(&self, function: u32) -> bool {
        // SAFETY: this image's resolver was installed before the host loop
        // started, and the bound function remains live with the library.
        unsafe { (self.main_thread_lifecycle_start)(function) != 0 }
    }

    /// Advances native lifecycles scheduled in a host-owned loop.
    pub fn pump_main_thread_lifecycles(&self, budget: u64) -> bool {
        // SAFETY: the bound function owns its thread-local scheduler state.
        unsafe { (self.main_thread_lifecycle_pump)(budget) != 0 }
    }

    /// Releases native lifecycle stacks at a host-run boundary.
    pub fn reset_main_thread_lifecycles(&self) {
        // SAFETY: the bound function takes no borrowed state.
        unsafe { (self.main_thread_lifecycle_reset)() };
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
}

impl Drop for NativeLibrary {
    fn drop(&mut self) {
        // SAFETY: dropping the session's last library wrapper happens after
        // active calls and callbacks have stopped, so no callee can read a
        // retained pointer again. The library remains loaded through this call.
        unsafe { (self.cblock_release_retained)() };
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
