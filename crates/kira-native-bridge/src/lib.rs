//! Native runtime support and the native<->VM bridge surface.
//!
//! Layer 4 of the Kira package graph. This crate is native-only and lives
//! outside the portable VM core: it is compiled to a static archive
//! (`libkira_native_bridge.a`) and linked into every native executable the
//! LLVM backend produces.
//!
//! Today it provides the [`runtime`] support library — the stable C-ABI helper
//! symbols that LLVM/native-lowered Kira code calls for `print` and string
//! values. Because those helpers format with the same Rust standard library the
//! VM uses, `print` output is identical byte-for-byte across `kira run` (VM) and
//! `kira build --backend llvm` (native). The hybrid native<->runtime bridge
//! (trampolines, the installed runtime invoker) is designed fresh alongside the
//! hybrid runtime and will live beside it here.

pub mod accounting;
pub mod array;
pub mod boxes;
pub mod cblock;
pub mod cells;
pub mod channels;
pub mod dynamic_call;
pub mod dynamic_library;
pub mod enums;
pub mod env;
pub mod file_system;
pub mod foreign;
pub mod hybrid;
pub mod live;
pub mod main_thread;
pub mod native_state;
mod pool;
pub mod raw_memory;
pub mod runtime;
pub mod state_box;
pub mod string_ops;
pub mod tasks;
pub mod traps;
pub mod values;

pub use array::{KArray, KiraArray};
pub use boxes::{kira_rt_box_free, kira_rt_box_new};
pub use cells::{
    KCell, kira_rt_cell_free, kira_rt_cell_get, kira_rt_cell_get_aggregate, kira_rt_cell_new,
    kira_rt_cell_new_aggregate, kira_rt_cell_set, kira_rt_cell_set_aggregate,
};
pub use enums::{KEnum, KiraEnum, PAYLOAD_ENUM, PAYLOAD_INERT, PAYLOAD_STR};
pub use foreign::{kira_foreign_adapter_abi_version_3, kira_rt_cstring_free, kira_rt_cstring_new};
pub use hybrid::{RuntimeInvoker, kira_hybrid_call_runtime, kira_hybrid_install_runtime_invoker};
pub use main_thread::{MainThreadDispatcher, MainThreadEntry};
pub use runtime::{KStr, KiraString};

/// Keeps this crate in a host binary's link graph.
///
/// A host that exports the `kira_dynamic_*` and `kira_live_*` symbols for a
/// loaded native half asks its linker for them by name. Rust reaches none of
/// them, and an extern crate no Rust code names is never handed to the linker,
/// so the request would go unanswered. Calling this from a binary's entry point
/// answers it.
pub fn retain_process_exports() {
    std::hint::black_box(live::kira_live_take_reload as extern "C" fn() -> bool);
}
