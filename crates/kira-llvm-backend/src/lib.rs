//! LLVM/native backend: compiles Kira IR to machine code via the LLVM
//! toolchain and clang driver.
//!
//! Layer 4 of the Kira package graph.
//! Ported from kira-zig `packages/kira_llvm_backend`; the module tree
//! mirrors the Zig file split one-to-one (test files excluded), with
//! `llvm_c.zig` becoming `llvm_api`.
//!
//! NO llvm-sys/inkwell — LLVM-C is dynamically loaded at RUN time via
//! libloading behind the `llvm-dylib` cargo feature during migration, so the
//! workspace always builds without an installed LLVM. See `llvm_api` for the
//! full design note.

// #![warn(missing_docs)] // enable once the port lands real code

pub mod backend;
pub mod backend_capi;
pub mod backend_capi_aggregate;
pub mod backend_capi_array_dtors;
pub mod backend_capi_bridge_string;
pub mod backend_capi_calls;
pub mod backend_capi_closure_dtors;
pub mod backend_capi_closures;
pub mod backend_capi_codegen;
pub mod backend_capi_copy;
pub mod backend_capi_debug;
pub mod backend_capi_destructors;
pub mod backend_capi_dispatch;
pub mod backend_capi_drop;
pub mod backend_capi_drop_slots;
pub mod backend_capi_dynamic_dtors;
pub mod backend_capi_enum;
pub mod backend_capi_enum_dtors;
pub mod backend_capi_ffi;
pub mod backend_capi_fresh_any;
pub mod backend_capi_native_state;
pub mod backend_capi_print;
pub mod backend_capi_returns;
pub mod backend_capi_struct_dtors;
pub mod backend_capi_tasks;
pub mod backend_capi_types;
pub mod backend_capi_value_repr;
pub mod backend_capi_wasm_cb_adapters;
pub mod backend_capi_wasm_native_cb;
pub mod backend_monomorphization;
pub mod backend_platform_utils;
pub mod backend_runtime_utils;
pub mod backend_utils;
pub mod cgu_build;
pub mod cgu_cache;
pub mod cgu_hash;
pub mod clang_driver;
pub mod debug_dwarf;
pub mod emscripten;
pub mod link;
pub mod llvm_api;
pub mod progress;
pub mod runtime_symbols;
pub mod stubs;
pub mod target;
pub mod toolchain;
pub mod types;

pub use llvm_api::{LlvmApi, LlvmRef};

/// Returns this crate's name (scaffold smoke check).
pub fn crate_name() -> &'static str {
    "kira-llvm-backend"
}
