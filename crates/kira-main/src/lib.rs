//! C ABI facade over the Kira runtime, built as staticlib/cdylib for embedders.
//!
//! Layer 10 of the Kira package graph.
//!
//! Design pending. This crate will expose the stable C entry points embedders
//! call to load and run Kira programs. The exported surface is designed fresh
//! with the new runtime; `#[repr(C)]` boundary types ship with layout tests.
