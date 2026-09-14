use kira_runtime_abi::BridgeValue;
use std::ffi::c_void;
use std::sync::Arc;

use super::RuntimeInvoker;

pub(super) type StrNewFn = unsafe extern "C" fn(data: *const u8, len: usize) -> *mut c_void;
pub(super) type StrFreeFn = unsafe extern "C" fn(value: *mut c_void);
pub(super) type StrDataFn = unsafe extern "C" fn(value: *mut c_void) -> *const u8;
pub(super) type StrLenFn = unsafe extern "C" fn(value: *mut c_void) -> usize;
pub(super) type HeapReportFn = unsafe extern "C" fn();
pub(super) type TaskResetFn = unsafe extern "C" fn();
pub(super) type ChannelTryFn = unsafe extern "C" fn(i64, i64, i64, i64, *mut i64) -> i64;
pub(super) type MainThreadRunFn = unsafe extern "C" fn(extern "C" fn() -> i32) -> i32;
pub(super) type MainThreadInstallDispatcherFn = unsafe extern "C" fn(*mut c_void);
pub(super) type MainThreadDispatcherFn =
    unsafe extern "C" fn(u32, *mut BridgeValue, u32, *mut BridgeValue);
pub(super) type MainThreadLifecycleResolverFn = unsafe extern "C" fn(u32) -> *mut c_void;
pub(super) type MainThreadLifecycleStartFn = unsafe extern "C" fn(u32) -> u8;
pub(super) type MainThreadLifecyclePumpFn = unsafe extern "C" fn(u64) -> u8;
pub(super) type MainThreadLifecycleResetFn = unsafe extern "C" fn();
pub(super) type LiveReloadMarkFn = unsafe extern "C" fn();
pub(super) type InstallInvokerFn = unsafe extern "C" fn(invoker: Option<RuntimeInvoker>);
pub(super) type StateNode = *mut c_void;
pub(super) type StateIntFn = unsafe extern "C" fn(i64) -> StateNode;
pub(super) type StateAnyFn = unsafe extern "C" fn(u64, StateNode) -> StateNode;
pub(super) type StateReadAnyTypeFn = unsafe extern "C" fn(StateNode) -> u64;
pub(super) type StateRawPtrFn = unsafe extern "C" fn(u64) -> StateNode;
pub(super) type StateNativeStateFn = unsafe extern "C" fn(u64) -> StateNode;
pub(super) type StateCellFn = unsafe extern "C" fn(u64) -> StateNode;
pub(super) type StateReadCellFn = unsafe extern "C" fn(StateNode) -> u64;
pub(super) type CellFreeFn = unsafe extern "C" fn(u64);
pub(super) type CellProxyNewFn = unsafe extern "C" fn(u64) -> u64;
pub(super) type CellProxyHandleFn = unsafe extern "C" fn(u64) -> u64;
pub(super) type StateFloatFn = unsafe extern "C" fn(f64) -> StateNode;
pub(super) type StateBoolFn = unsafe extern "C" fn(u8) -> StateNode;
pub(super) type StateStringFn = unsafe extern "C" fn(*mut c_void) -> StateNode;
pub(super) type StateAggregateFn = unsafe extern "C" fn(u32, u32, usize) -> StateNode;
pub(super) type StateSetChildFn = unsafe extern "C" fn(StateNode, usize, StateNode) -> u32;
pub(super) type StateTagFn = unsafe extern "C" fn(StateNode) -> u32;
pub(super) type StateReadIntFn = unsafe extern "C" fn(StateNode) -> i64;
pub(super) type StateNumberFn = unsafe extern "C" fn(i128) -> StateNode;
pub(super) type StateReadNumberFn = unsafe extern "C" fn(StateNode) -> i128;
pub(super) type StateReadRawPtrFn = unsafe extern "C" fn(StateNode) -> u64;
pub(super) type StateReadNativeStateFn = unsafe extern "C" fn(StateNode) -> u64;
pub(super) type StateReadDropGlueFn = unsafe extern "C" fn(StateNode) -> u32;
pub(super) type StateReadFloatFn = unsafe extern "C" fn(StateNode) -> f64;
pub(super) type StateReadBoolFn = unsafe extern "C" fn(StateNode) -> u8;
pub(super) type StateReadStringFn = unsafe extern "C" fn(StateNode) -> *mut c_void;
pub(super) type StateLenFn = unsafe extern "C" fn(StateNode) -> usize;
pub(super) type StateCBlockFn = unsafe extern "C" fn(*const u8, usize, usize) -> StateNode;
pub(super) type StateSetCBlockChildFn =
    unsafe extern "C" fn(StateNode, usize, u64, u32, StateNode) -> u32;
pub(super) type StateReadCBlockLenFn = unsafe extern "C" fn(StateNode) -> usize;
pub(super) type StateReadCBlockDataFn = unsafe extern "C" fn(StateNode) -> *const u8;
pub(super) type StateReadCBlockChildOffsetFn = unsafe extern "C" fn(StateNode, usize) -> u64;
pub(super) type StateReadCBlockChildWidthFn = unsafe extern "C" fn(StateNode, usize) -> u32;
pub(super) type StateEnumTagFn = unsafe extern "C" fn(StateNode) -> u32;
pub(super) type StateChildFn = unsafe extern "C" fn(StateNode, usize) -> StateNode;
pub(super) type StateNodeFreeFn = unsafe extern "C" fn(StateNode);
pub(super) type StateNewFn = unsafe extern "C" fn(u64, StateNode, *mut u64) -> u32;
pub(super) type StateNewDroppingFn = unsafe extern "C" fn(u64, StateNode, u32, *mut u64) -> u32;
pub(super) type StateRecoverFn = unsafe extern "C" fn(u64, u64, *mut StateNode) -> u32;
pub(super) type StateReplaceFn = unsafe extern "C" fn(u64, u64, StateNode, *mut StateNode) -> u32;
pub(super) type StateCountFn = unsafe extern "C" fn(u64) -> u32;
pub(super) type StateReleaseDroppingFn = unsafe extern "C" fn(u64, *mut StateNode) -> u32;
pub(super) type CBlockReleaseRetainedFn = unsafe extern "C" fn();
/// The loaded library lease carried by a decoded cell's release closure.
pub(super) struct CellReleaseOwner<L> {
    /// Keeps the image containing `free` loaded until all cell shares release.
    _library: Arc<L>,
    /// Resolved from the library held by `_library`.
    pub(super) free: CellFreeFn,
}

impl<L> CellReleaseOwner<L> {
    pub(super) fn new(library: Arc<L>, free: CellFreeFn) -> CellReleaseOwner<L> {
        CellReleaseOwner {
            _library: library,
            free,
        }
    }

    pub(super) fn release(&self, handle: u64) {
        // SAFETY: `free` came from the library held by this owner, and the
        // owner remains alive for the duration of the call.
        unsafe { (self.free)(handle) };
    }
}

/// The symbols every hybrid library must export, whatever the program does.
pub(super) const STR_NEW: &[u8] = b"kira_rt_str_new\0";
pub(super) const STR_FREE: &[u8] = b"kira_rt_str_free\0";
pub(super) const STR_DATA: &[u8] = b"kira_rt_str_data\0";
pub(super) const STR_LEN: &[u8] = b"kira_rt_str_len\0";
pub(super) const INSTALL_INVOKER: &[u8] = b"kira_hybrid_install_runtime_invoker\0";
pub(super) const LIVE_RELOAD_MARK: &[u8] = b"kira_live_mark_reload\0";
/// Optional, unlike the rest: an older library simply has no accounting.
pub(super) const HEAP_REPORT: &[u8] = b"kira_rt_heap_report\0";
pub(super) const TASK_RESET: &[u8] = b"kira_rt_task_reset\0";
pub(super) const CHANNEL_RESET: &[u8] = b"kira_rt_channel_reset\0";
pub(super) const CHANNEL_TRY: &[u8] = b"kira_rt_channel_try\0";
pub(super) const MAIN_THREAD_RUN: &[u8] = b"kira_rt_main_thread_run\0";
pub(super) const MAIN_THREAD_INSTALL_DISPATCHER: &[u8] =
    b"kira_rt_main_thread_install_dispatcher\0";
pub(super) const MAIN_THREAD_DISPATCHER: &[u8] = b"kira_main_thread_dispatch\0";
pub(super) const MAIN_THREAD_INSTALL_LIFECYCLE_RESOLVER: &[u8] =
    b"kira_rt_main_thread_install_lifecycle_resolver\0";
pub(super) const MAIN_THREAD_LIFECYCLE_RESOLVER: &[u8] = b"kira_main_thread_lifecycle_resolve\0";
pub(super) const MAIN_THREAD_LIFECYCLE_START: &[u8] =
    b"kira_rt_main_thread_lifecycle_start_local\0";
pub(super) const MAIN_THREAD_LIFECYCLE_PUMP: &[u8] = b"kira_rt_main_thread_lifecycle_pump_local\0";
pub(super) const MAIN_THREAD_LIFECYCLE_RESET: &[u8] =
    b"kira_rt_main_thread_lifecycle_reset_local\0";
pub(super) const STATE_VALUE_INT: &[u8] = b"kira_rt_native_value_int\0";
pub(super) const STATE_VALUE_ANY: &[u8] = b"kira_rt_native_value_any\0";
pub(super) const STATE_VALUE_READ_ANY_TYPE: &[u8] = b"kira_rt_native_value_read_any_type\0";
pub(super) const STATE_VALUE_RAW_PTR: &[u8] = b"kira_rt_native_value_raw_ptr\0";
pub(super) const STATE_VALUE_NATIVE_STATE: &[u8] = b"kira_rt_native_value_native_state\0";
pub(super) const CELL_FREE: &[u8] = b"kira_rt_cell_free\0";
pub(super) const CELL_VM_PROXY_NEW: &[u8] = b"kira_rt_cell_vm_proxy_new\0";
pub(super) const CELL_VM_PROXY_HANDLE: &[u8] = b"kira_rt_cell_vm_proxy_handle\0";
pub(super) const STATE_VALUE_CELL: &[u8] = b"kira_rt_native_value_cell\0";
pub(super) const STATE_VALUE_READ_CELL: &[u8] = b"kira_rt_native_value_read_cell\0";
pub(super) const STATE_VALUE_FLOAT: &[u8] = b"kira_rt_native_value_float\0";
pub(super) const STATE_VALUE_BOOL: &[u8] = b"kira_rt_native_value_bool\0";
pub(super) const STATE_VALUE_STRING: &[u8] = b"kira_rt_native_value_string\0";
pub(super) const STATE_VALUE_AGGREGATE: &[u8] = b"kira_rt_native_value_aggregate\0";
pub(super) const STATE_VALUE_SET_CHILD: &[u8] = b"kira_rt_native_value_set_child\0";
pub(super) const STATE_VALUE_TAG: &[u8] = b"kira_rt_native_value_tag\0";
pub(super) const STATE_VALUE_READ_INT: &[u8] = b"kira_rt_native_value_read_int\0";
pub(super) const STATE_VALUE_NUMBER: &[u8] = b"kira_rt_native_value_number\0";
pub(super) const STATE_VALUE_READ_NUMBER: &[u8] = b"kira_rt_native_value_read_number\0";
pub(super) const STATE_VALUE_READ_RAW_PTR: &[u8] = b"kira_rt_native_value_read_raw_ptr\0";
pub(super) const STATE_VALUE_READ_NATIVE_STATE: &[u8] = b"kira_rt_native_value_read_native_state\0";
pub(super) const STATE_VALUE_READ_DROP_GLUE: &[u8] = b"kira_rt_native_value_read_drop_glue\0";
pub(super) const STATE_VALUE_READ_FLOAT: &[u8] = b"kira_rt_native_value_read_float\0";
pub(super) const STATE_VALUE_READ_BOOL: &[u8] = b"kira_rt_native_value_read_bool\0";
pub(super) const STATE_VALUE_READ_STRING: &[u8] = b"kira_rt_native_value_read_string\0";
pub(super) const STATE_VALUE_LEN: &[u8] = b"kira_rt_native_value_len\0";
pub(super) const STATE_VALUE_CBLOCK: &[u8] = b"kira_rt_native_value_cblock\0";
pub(super) const STATE_VALUE_SET_CBLOCK_CHILD: &[u8] = b"kira_rt_native_value_set_cblock_child\0";
pub(super) const STATE_VALUE_READ_CBLOCK_LEN: &[u8] = b"kira_rt_native_value_read_cblock_len\0";
pub(super) const STATE_VALUE_READ_CBLOCK_DATA: &[u8] = b"kira_rt_native_value_read_cblock_data\0";
pub(super) const STATE_VALUE_CBLOCK_CHILD_OFFSET: &[u8] =
    b"kira_rt_native_value_cblock_child_offset\0";
pub(super) const STATE_VALUE_CBLOCK_CHILD_WIDTH: &[u8] =
    b"kira_rt_native_value_cblock_child_width\0";
pub(super) const STATE_VALUE_ENUM_TAG: &[u8] = b"kira_rt_native_value_enum_tag\0";
pub(super) const STATE_VALUE_CHILD: &[u8] = b"kira_rt_native_value_child\0";
pub(super) const STATE_VALUE_FREE: &[u8] = b"kira_rt_native_value_free\0";
pub(super) const STATE_NEW: &[u8] = b"kira_rt_native_state_new\0";
pub(super) const STATE_NEW_DROPPING: &[u8] = b"kira_rt_native_state_new_dropping\0";
pub(super) const STATE_RECOVER: &[u8] = b"kira_rt_native_state_recover\0";
pub(super) const STATE_REPLACE: &[u8] = b"kira_rt_native_state_replace\0";
pub(super) const STATE_RETAIN: &[u8] = b"kira_rt_native_state_retain\0";
pub(super) const STATE_RELEASE: &[u8] = b"kira_rt_native_state_release\0";
pub(super) const STATE_RELEASE_DROPPING: &[u8] = b"kira_rt_native_state_release_dropping\0";
pub(super) const CBLOCK_RELEASE_RETAINED: &[u8] = b"kira_rt_cblock_release_retained\0";
