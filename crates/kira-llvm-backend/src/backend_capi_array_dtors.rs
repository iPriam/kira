//! Emits array destructor functions: element-wise release loops matching
//! the VM heap's array drop semantics.
//!
//! Ported from kira-zig
//! `packages/kira_llvm_backend/src/backend_capi_array_dtors.zig`.
