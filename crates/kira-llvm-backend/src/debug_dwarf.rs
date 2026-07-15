//! DWARF emission: DIBuilder-based compile units, line locations, and
//! dbg.declare records (LLVM 22 records form), degrading gracefully when
//! the optional DIBuilder surface is absent (see `llvm_api`).
//!
//! Ported from kira-zig `packages/kira_llvm_backend/src/debug_dwarf.zig`.
