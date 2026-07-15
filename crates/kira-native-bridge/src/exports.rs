//! The C runtime-helper surface linked into every native Kira build.
//!
//! Ported from kira-zig `packages/kira_native_bridge/src/runtime_helpers.c`
//! (+ `runtime_helpers_arrays.inc`, `runtime_helpers_native_state.inc`,
//! `runtime_helpers_tasks.inc`). In Zig these are C functions compiled into
//! the native artifact; in the Rust port they become `extern "C"` functions
//! exported from the runtime staticlib.
//!
//! NO symbols are exported at scaffold time — every function here is a
//! signature scaffold WITHOUT `#[no_mangle]`/`#[export_name]`, so this crate
//! contributes no linker-visible names yet. When the port lands, the
//! following list gains `#[unsafe(no_mangle)]` (exact spellings from the
//! `KIRA_BRIDGE_EXPORT` markers in the C source):
//!
//! - Arrays: `kira_array_alloc`, `kira_array_append`, `kira_array_clone`,
//!   `kira_array_len`, `kira_array_load`, `kira_array_release`,
//!   `kira_array_release_replaced`, `kira_array_store`,
//!   `kira_array_store_release`, `kira_array_take`
//! - Strings: `kira_string_char_at`, `kira_string_from_bool`,
//!   `kira_string_from_f64`, `kira_string_from_i64`, `kira_string_index_of`,
//!   `kira_string_substring`
//! - Structs: `kira_struct_alloc`, `kira_struct_free`, `kira_struct_type_id`
//! - Native state: `kira_native_state_alloc`, `kira_native_state_free`,
//!   `kira_native_state_payload`, `kira_native_state_recover`,
//!   `kira_capi_install_state_interior_release`
//! - Closures: `kira_destroy_closure`
//! - Print/write: `kira_native_print_f64`, `kira_native_print_i64`,
//!   `kira_native_print_string`, `kira_native_write_f64`,
//!   `kira_native_write_i64`, `kira_native_write_newline`,
//!   `kira_native_write_ptr`, `kira_native_write_string`
//! - Hybrid hooks: `kira_hybrid_call_runtime`,
//!   `kira_hybrid_install_array_allocator`,
//!   `kira_hybrid_install_closure_destroy`,
//!   `kira_hybrid_install_runtime_invoker`,
//!   `kira_set_execution_trace_enabled`, `kira_native_bridge`
//! - Live: `kira_live_emit_first_frame`, `kira_live_emit_log_line`,
//!   `kira_live_install_first_frame_hook`, `kira_live_install_log_hook`
//! - Tasks: `kira_task_alloc_args`, `kira_task_await`, `kira_task_cancel`,
//!   `kira_task_detach`, `kira_task_drain_all`, `kira_task_is_complete`,
//!   `kira_task_sleep`, `kira_task_spawn`, `kira_task_spawn_ready`,
//!   `kira_task_spawn_suspendable`, `kira_task_yield`

use crate::abi::{KiraArray, KiraBridgeValue};

/// Number of live elements in `array` (0 for null).
/// C: `size_t kira_array_len(const KiraArray *array)`.
/// Representative signature scaffold — not exported yet (see module docs).
pub extern "C" fn kira_array_len(_array: *const KiraArray) -> usize {
    todo!("ported with the runtime_helpers migration")
}

/// Append `value` to `array`, growing geometrically through `cap`.
/// C: `void kira_array_append(KiraArray *array, KiraBridgeValue value)`.
/// Representative signature scaffold — not exported yet (see module docs).
pub extern "C" fn kira_array_append(_array: *mut KiraArray, _value: KiraBridgeValue) {
    todo!("ported with the runtime_helpers migration")
}

/// Release `array` and every managed element it owns.
/// C: `void kira_array_release(KiraArray *array)`.
/// Representative signature scaffold — not exported yet (see module docs).
pub extern "C" fn kira_array_release(_array: *mut KiraArray) {
    todo!("ported with the runtime_helpers migration")
}
