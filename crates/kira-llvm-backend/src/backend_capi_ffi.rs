//! Codegen for FFI calls: lowering declared foreign signatures to direct
//! C calls with ownership-annotated argument marshaling.
//!
//! Ported from kira-zig
//! `packages/kira_llvm_backend/src/backend_capi_ffi.zig`.
