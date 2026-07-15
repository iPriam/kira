//! The per-function instruction lowering loop: walks Kira IR basic blocks
//! and dispatches each instruction to its emitter.
//!
//! Ported from kira-zig
//! `packages/kira_llvm_backend/src/backend_capi_codegen.zig`.
