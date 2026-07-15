//! The Kira VM: bytecode interpreter and runtime.
//!
//! Layer 4 of the Kira package graph.
//!
//! Design pending. Standing constraints for the design:
//!
//! - **Portable core.** No filesystem, process, thread, or dynamic-loading
//!   calls; the crate must compile for `wasm32-unknown-unknown`. It consumes
//!   bytes and talks to the world through a host-capabilities trait (print,
//!   clock, rng, …) supplied by the embedder. Native-only functionality
//!   (dynamic FFI, dlopen) is feature-gated and lives outside this core.
//! - **Affine ownership.** The runtime enforces Rust-style affine ownership
//!   with leak accounting, so dropped values are provably reclaimed.
//! - **Interpreter is hot.** Built `opt-level = 3` even in dev; dispatch is
//!   match-in-loop, and hot paths do not allocate.
