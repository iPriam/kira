//! CGU object cache: content-hash keyed reuse of compiled objects across
//! builds (`.o` retention also feeds native debugging).
//!
//! Ported from kira-zig `packages/kira_llvm_backend/src/cgu_cache.zig`.
