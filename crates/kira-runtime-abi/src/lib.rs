//! Runtime value ABI shared across the VM and native backends.
//!
//! Layer 0 of the Kira package graph.
//!
//! Design pending. This crate will own the single definition of the runtime
//! value representation and the host-interface value types, designed fresh
//! with the new VM. Bytes foreign code can write are modeled as transparent
//! newtypes with associated consts (never Rust enums), and every `#[repr(C)]`
//! type ships with a layout test.
