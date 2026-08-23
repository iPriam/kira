//! The small libffi C surface Kira uses.
//!
//! libffi is linked into this crate statically, so these are ordinary `extern`
//! declarations rather than symbols looked up in a library opened at run time.
//! That is the difference between a Kira program that carries its engine and
//! one that has to find it: there is no file to ship beside an artifact, no
//! search path that could resolve to a different libffi, and no failure mode
//! where the engine is missing — a program that links is a program that has it.
//!
//! `ffi_get_default_abi` and `ffi_get_closure_size` are Kira additions to the
//! libffi fork (`src/types.c`). Upstream exposes both only as macros, which a
//! Rust caller cannot reach through the ABI.

use std::ffi::c_void;

use crate::LibffiError;

pub(crate) const FFI_TYPE_STRUCT: u16 = 13;

#[repr(C)]
pub(crate) struct RawFfiType {
    pub(crate) size: usize,
    pub(crate) alignment: u16,
    pub(crate) kind: u16,
    pub(crate) elements: *mut *mut RawFfiType,
}

#[repr(C)]
pub struct RawFfiCif {
    pub(crate) abi: u32,
    pub(crate) nargs: u32,
    pub(crate) arg_types: *mut *mut RawFfiType,
    pub(crate) result_type: *mut RawFfiType,
    pub(crate) bytes: u32,
    pub(crate) flags: u32,
}

pub(crate) type RawFunction = unsafe extern "C" fn();
pub(crate) type ClosureCallback = unsafe extern "C" fn(
    cif: *mut RawFfiCif,
    result: *mut c_void,
    arguments: *mut *mut c_void,
    user_data: *mut c_void,
);

type PrepCif = unsafe extern "C" fn(
    cif: *mut RawFfiCif,
    abi: u32,
    nargs: u32,
    result_type: *mut RawFfiType,
    argument_types: *mut *mut RawFfiType,
) -> i32;
type Call = unsafe extern "C" fn(
    cif: *mut RawFfiCif,
    function: RawFunction,
    result: *mut c_void,
    arguments: *mut *mut c_void,
);
type ClosureAlloc = unsafe extern "C" fn(size: usize, code: *mut *mut c_void) -> *mut c_void;
type ClosureFree = unsafe extern "C" fn(closure: *mut c_void);
type PrepClosure = unsafe extern "C" fn(
    closure: *mut c_void,
    cif: *mut RawFfiCif,
    callback: ClosureCallback,
    user_data: *mut c_void,
    code: *mut c_void,
) -> i32;
type ClosureSize = unsafe extern "C" fn() -> usize;
type DefaultAbi = unsafe extern "C" fn() -> u32;

/// The libffi entry points and standard scalar type descriptors.
///
/// Kept as a table of pointers rather than called directly, because everything
/// above it was written against a table and the indirection costs one load.
pub(crate) struct RawLibffi {
    pub(crate) prep_cif: PrepCif,
    pub(crate) call: Call,
    pub(crate) closure_alloc: ClosureAlloc,
    pub(crate) closure_free: ClosureFree,
    pub(crate) prep_closure_loc: PrepClosure,
    pub(crate) closure_size: ClosureSize,
    pub(crate) default_abi: DefaultAbi,
    pub(crate) type_void: *mut RawFfiType,
    pub(crate) type_uint8: *mut RawFfiType,
    pub(crate) type_sint8: *mut RawFfiType,
    pub(crate) type_uint16: *mut RawFfiType,
    pub(crate) type_sint16: *mut RawFfiType,
    pub(crate) type_uint32: *mut RawFfiType,
    pub(crate) type_sint32: *mut RawFfiType,
    pub(crate) type_uint64: *mut RawFfiType,
    pub(crate) type_sint64: *mut RawFfiType,
    pub(crate) type_float: *mut RawFfiType,
    pub(crate) type_double: *mut RawFfiType,
    pub(crate) type_pointer: *mut RawFfiType,
}

// SAFETY: `RawLibffi` stores function pointers plus libffi's process-global
// scalar type descriptors, all of them linked into this image and therefore
// valid for its whole life. The descriptors are immutable after startup; every
// CIF, aggregate graph, and closure allocation that libffi mutates is owned by
// the caller and never shared through this value.
unsafe impl Send for RawLibffi {}

// SAFETY: the same invariant as `Send` applies to concurrent readers. Libffi's
// call, CIF preparation, and closure preparation APIs receive their mutable
// state through caller-owned pointers, while this table is read-only.
unsafe impl Sync for RawLibffi {}

unsafe extern "C" {
    fn ffi_prep_cif(
        cif: *mut RawFfiCif,
        abi: u32,
        nargs: u32,
        result_type: *mut RawFfiType,
        argument_types: *mut *mut RawFfiType,
    ) -> i32;
    fn ffi_call(
        cif: *mut RawFfiCif,
        function: RawFunction,
        result: *mut c_void,
        arguments: *mut *mut c_void,
    );
    fn ffi_closure_alloc(size: usize, code: *mut *mut c_void) -> *mut c_void;
    fn ffi_closure_free(closure: *mut c_void);
    fn ffi_prep_closure_loc(
        closure: *mut c_void,
        cif: *mut RawFfiCif,
        callback: ClosureCallback,
        user_data: *mut c_void,
        code: *mut c_void,
    ) -> i32;
    fn ffi_get_closure_size() -> usize;
    fn ffi_get_default_abi() -> u32;

    static ffi_type_void: RawFfiType;
    static ffi_type_uint8: RawFfiType;
    static ffi_type_sint8: RawFfiType;
    static ffi_type_uint16: RawFfiType;
    static ffi_type_sint16: RawFfiType;
    static ffi_type_uint32: RawFfiType;
    static ffi_type_sint32: RawFfiType;
    static ffi_type_uint64: RawFfiType;
    static ffi_type_sint64: RawFfiType;
    static ffi_type_float: RawFfiType;
    static ffi_type_double: RawFfiType;
    static ffi_type_pointer: RawFfiType;
}

impl RawLibffi {
    /// The linked engine.
    ///
    /// Infallible, and that is the point of linking it: the old form could fail
    /// because the engine was a file that might not be there. Kept returning a
    /// `Result` so callers that already handle one are unchanged, and so a
    /// target Kira publishes no archive for can fail here rather than fail to
    /// link.
    pub(crate) fn load() -> Result<Self, LibffiError> {
        Ok(Self::linked())
    }

    /// The table, built from the symbols linked into this image.
    pub(crate) fn linked() -> Self {
        Self {
            prep_cif: ffi_prep_cif,
            call: ffi_call,
            closure_alloc: ffi_closure_alloc,
            closure_free: ffi_closure_free,
            prep_closure_loc: ffi_prep_closure_loc,
            closure_size: ffi_get_closure_size,
            default_abi: ffi_get_default_abi,
            // `&raw const` rather than `&`: these are `extern` statics, and
            // taking a reference to one asserts an initialised Rust value where
            // what exists is a C object this code only ever hands back to C.
            // The casts drop `const` because libffi's own signatures take
            // `ffi_type *`, not `const ffi_type *`.
            type_void: (&raw const ffi_type_void).cast_mut(),
            type_uint8: (&raw const ffi_type_uint8).cast_mut(),
            type_sint8: (&raw const ffi_type_sint8).cast_mut(),
            type_uint16: (&raw const ffi_type_uint16).cast_mut(),
            type_sint16: (&raw const ffi_type_sint16).cast_mut(),
            type_uint32: (&raw const ffi_type_uint32).cast_mut(),
            type_sint32: (&raw const ffi_type_sint32).cast_mut(),
            type_uint64: (&raw const ffi_type_uint64).cast_mut(),
            type_sint64: (&raw const ffi_type_sint64).cast_mut(),
            type_float: (&raw const ffi_type_float).cast_mut(),
            type_double: (&raw const ffi_type_double).cast_mut(),
            type_pointer: (&raw const ffi_type_pointer).cast_mut(),
        }
    }
}
