//! libffi-backed dynamic calls: scalar storage, call values, and (at
//! migration) the dynamically-loaded libffi function table + prepared CIFs.
//!
//! Ported from kira-zig `packages/kira_dynamic_ffi/src/libffi.zig`.
//!
//! # libffi binding decision (deferred)
//!
//! No libffi crate dependency is added at scaffold time. The Zig
//! implementation does NOT link libffi: it dlopens it at runtime through
//! `DynamicLibrary` and resolves `ffi_prep_cif`, `ffi_call`, and the
//! `ffi_type_*` globals into a function-pointer table, with `FfiCifStorage`
//! as an opaque 32-word CIF buffer so no libffi headers are needed. The port
//! decides between (a) the same libloading-based table — zero build deps,
//! identical behavior, works with any system libffi — and (b) the `libffi`
//! crate (which builds/links libffi) if static linking wins. That decision
//! lands with the migration of the call path; the storage types below are
//! binding-agnostic either way.

use crate::signature;

/// Opaque storage for a libffi CIF, sized generously so no libffi headers
/// are required. Zig: `FfiCifStorage` (`extern struct { words: [32]usize }`).
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct FfiCifStorage {
    /// Zig: `words: [32]usize align(@alignOf(usize))`.
    pub words: [usize; 32],
}

impl std::fmt::Debug for FfiCifStorage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("FfiCifStorage(..)")
    }
}

/// Scalar argument/return storage, one slot per call value.
/// Zig: `ScalarStorage` (`extern union`).
#[repr(C)]
#[derive(Clone, Copy)]
pub union ScalarStorage {
    pub i8_: i8,
    pub u8_: u8,
    pub i16_: i16,
    pub u16_: u16,
    pub i32_: i32,
    pub u32_: u32,
    pub i64_: i64,
    pub u64_: u64,
    pub f32_: f32,
    pub f64_: f64,
    /// Zig: `pointer: usize`.
    pub pointer: usize,
}

/// A typed call value: FFI type plus its scalar storage. The libffi call
/// path passes `&storage` for non-void values. Zig: `Value` in `libffi.zig`
/// (re-exported as `LibffiValue`).
pub struct CallValue {
    /// Zig: `ty: sig.Type`.
    pub ty: signature::Type,
    /// Zig: `storage: ScalarStorage`.
    pub storage: ScalarStorage,
}

impl std::fmt::Debug for CallValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CallValue").field("ty", &self.ty).finish()
    }
}
