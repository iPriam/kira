//! Declarative macro expansion (frontend AST -> AST). Runs after parsing /
//! import merging and before semantic analysis; because the output is
//! ordinary syntax AST, all backends (VM, LLVM/native, hybrid, WASM) see
//! identical post-expansion code — macro parity is structural.
//!
//! Port target: kira-zig `kira_build/src/macro_expand.zig`.
