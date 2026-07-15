//! Codegen-unit builds: compiles one CGU to an object file (in-process
//! -O2 emission via the pass runner; no clang subprocess, no textual-IR
//! round trip).
//!
//! Ported from kira-zig `packages/kira_llvm_backend/src/cgu_build.zig`.
