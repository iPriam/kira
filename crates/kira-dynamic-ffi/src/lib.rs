//! Dynamic library loading for native FFI.
//!
//! Layer 0 of the Kira package graph. This crate owns native library handles;
//! calls through them use Kira's bundled libffi graph rather than generated C
//! ABI marshalling.

pub mod dynamic_library;
pub mod library;

/// The relocatable manifest token for a symbol exported by the host process.
pub const PROCESS_BINDING_MARKER: &str = "@kira-process";
/// The manifest token for a native half linked into the host process itself.
///
/// An embedded application links its program's native code directly into its
/// own binary; its hybrid manifest records this token instead of a file name,
/// and the loader binds trampolines from the running image rather than
/// opening one. The image must actually export those symbols — an embedded
/// link carries `-Wl,-export_dynamic` for exactly this reason.
pub const SELF_LIBRARY_MARKER: &str = "@kira-self";
/// The native bridge library name whose symbols are supplied by the host
/// process rather than by a load-time DLL.
pub const HOST_RUNTIME_LIBRARY: &str = "kira_runtime";

pub use dynamic_library::{DynamicLibrary, FfiError, open_process_image, open_shared_library};
pub use library::{ForeignLibrary, ForeignLibraryError};
