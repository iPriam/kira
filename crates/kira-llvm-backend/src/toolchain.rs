//! LLVM toolchain discovery: locates the LLVM-C dylib and companion tools
//! (per the documented discovery order), records init-symbol names per
//! target, and hands `llvm_api` what to open.
//!
//! Ported from kira-zig `packages/kira_llvm_backend/src/toolchain.zig`.
