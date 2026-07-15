//! Root of the C-API codegen family: shared state (`Codegen` context,
//! value maps, block stack) threaded through every `backend_capi_*` module.
//!
//! Ported from kira-zig `packages/kira_llvm_backend/src/backend_capi.zig`.
