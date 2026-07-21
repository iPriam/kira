//! Dynamic library loading for native FFI.
//!
//! Layer 0 of the Kira package graph. This crate loads only Kira-generated,
//! versioned uniform adapters; it does not construct arbitrary C signatures or
//! expose function pointers to bytecode.

pub mod adapter;
pub mod dynamic_library;

pub use adapter::{ForeignAdapterError, ForeignAdapterLibrary};
pub use dynamic_library::{DynamicLibrary, FfiError};
