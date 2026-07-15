//! Native library definitions: FFI symbols, link extras, and per-target resolution.
//!
//! Layer 3 of the Kira package graph.
//! Ported from kira-zig `packages/kira_native_lib_definition`.

pub mod ffi_symbol;
pub mod link_extras;
pub mod native_library;
pub mod target_resolution;

pub use ffi_symbol::NativeSymbol;
pub use link_extras::LinkExtras;
pub use native_library::{
    AutobindingBindings, AutobindingMode, AutobindingProfile, AutobindingSpec, BuildRecipe,
    FfiModuleSpec, HeaderSpec, LibraryAbi, LinkMode, NativeLibrarySpec, TargetSpec,
};
pub use target_resolution::{
    AssetMount, ResolvedNativeLibrary, TargetSelector, Unavailable, UnavailableReason,
};
