//! Bytecode format and compiler for the Kira VM.
//!
//! Layer 4 of the Kira package graph.
//!
//! Design pending. The module format, opcode set, and encoding are designed
//! fresh for the new VM. Wire formats are append-only once fixed. This crate is
//! part of the portable core: no filesystem, process, thread, or
//! dynamic-loading calls, and it must compile for `wasm32-unknown-unknown`.
