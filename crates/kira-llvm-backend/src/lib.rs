//! LLVM/native backend: compiles Kira IR to machine code via LLVM.
//!
//! Layer 4 of the Kira package graph.
//!
//! Design pending. Standing decisions for the design:
//!
//! - LLVM is reached through `llvm-sys` with dynamic loading via `libloading`
//!   — never `inkwell`, and never a static link against an LLVM dylib.
//! - The backend is feature-gated so the workspace builds without a local LLVM.
//! - `unsafe` is fenced to this crate's binding layer, with a `// SAFETY:`
//!   comment on every block.
