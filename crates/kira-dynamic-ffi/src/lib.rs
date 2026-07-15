//! Dynamic library loading and runtime FFI call signatures.
//!
//! Layer 0 of the Kira package graph.
//! Ported from kira-zig `packages/kira_dynamic_ffi` (`dynamic_library.zig`,
//! `signature.zig`, `libffi.zig`).

// #![warn(missing_docs)] // enable once the port lands real code

pub mod dynamic_library;
pub mod libffi;
pub mod signature;

pub use dynamic_library::{DynamicLibrary, FfiError};
pub use libffi::{CallValue, FfiCifStorage, ScalarStorage};
pub use signature::{
    Abi, ArrayType, BitflagsType, Callback, Diagnostic, DiagnosticCode, EnumType, Field,
    HandleType, IntBacking, Ownership, Parameter, PointerType, Signature, StructType, Type,
    UnionType,
};

/// Returns this crate's name (scaffold smoke check).
pub fn crate_name() -> &'static str {
    "kira-dynamic-ffi"
}
