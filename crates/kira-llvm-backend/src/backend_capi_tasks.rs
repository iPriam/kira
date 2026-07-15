//! Codegen for task opcodes: kira_task_* runtime calls (spawn variants,
//! await/cancel/detach/yield) and suspendable thunk emission.
//!
//! Ported from kira-zig
//! `packages/kira_llvm_backend/src/backend_capi_tasks.zig`.
