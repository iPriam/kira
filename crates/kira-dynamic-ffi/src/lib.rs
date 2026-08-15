//! Dynamic library loading for native FFI.
//!
//! Layer 0 of the Kira package graph. This crate owns native library handles;
//! calls through them use Kira's bundled libffi graph rather than generated C
//! ABI marshalling.

pub mod dynamic_library;
pub mod library;

/// The relocatable manifest token for a symbol exported by the host process.
pub const PROCESS_BINDING_MARKER: &str = "@kira-process";
/// The native bridge library name whose symbols are supplied by the host
/// process rather than by a load-time DLL.
pub const HOST_RUNTIME_LIBRARY: &str = "kira_runtime";

pub use dynamic_library::{DynamicLibrary, FfiError, open_shared_library};
pub use library::{ForeignLibrary, ForeignLibraryError};
