//! Codegen for bulk slot drops: frame-exit release of tracked slots,
//! mirroring the VM interpreter's releaseTrackedSlots.
//!
//! Ported from kira-zig
//! `packages/kira_llvm_backend/src/backend_capi_drop_slots.zig`.
