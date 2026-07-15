//! Dynamic library loading for native FFI.
//!
//! Layer 0 of the Kira package graph. The runtime call-signature model and the
//! libffi calling path are designed fresh alongside the new VM; for now this
//! crate provides the cross-platform shared-library handle.

pub mod dynamic_library;

pub use dynamic_library::{DynamicLibrary, FfiError};
