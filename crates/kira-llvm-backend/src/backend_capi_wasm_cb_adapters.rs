//! wasm32 callback adapters: shims fixing the i64-width mismatches between
//! Emscripten's JS boundary and the native callback ABI.
//!
//! Ported from kira-zig
//! `packages/kira_llvm_backend/src/backend_capi_wasm_cb_adapters.zig`.
