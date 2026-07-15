//! Destructor orchestration: selects/emits the right dtor family
//! (array/closure/enum/struct/dynamic) per type and wires drop calls.
//!
//! Ported from kira-zig
//! `packages/kira_llvm_backend/src/backend_capi_destructors.zig`.
