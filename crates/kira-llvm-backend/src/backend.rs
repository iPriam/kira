//! The native backend entry point: orchestrates IR -> LLVM module
//! codegen, CGU partitioning/caching, optimization, object emission, and
//! linking into the final binary.
//!
//! Ported from kira-zig `packages/kira_llvm_backend/src/backend.zig`.
