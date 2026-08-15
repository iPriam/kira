//! `ffi_closure` ownership for C callbacks.

use std::ffi::c_void;
use std::mem::align_of;

use kira_runtime_abi::{ForeignAggregates, ForeignSignature};

use crate::LibffiError;
use crate::call::LibffiRuntime;
use crate::raw::{ClosureCallback, RawFfiCif};
use crate::types::PreparedCif;

/// The callback libffi enters after it has decoded the C ABI arguments.
pub type FfiClosureCallback = ClosureCallback;

/// An executable libffi closure held alive for as long as C may call it.
pub struct FfiClosure {
    runtime: std::sync::Arc<crate::raw::RawLibffi>,
    /// Libffi retains this address in the executable closure, so the prepared
    /// CIF must not move when the owning registry or thread moves this value.
    prepared: Box<PreparedCif>,
    closure: *mut c_void,
    code: *mut c_void,
    user_data: *mut c_void,
}

// SAFETY: the executable closure allocation and its boxed CIF graph are owned
// by this value, so moving it does not change any address libffi stored. The
// caller-owned user-data address is retained unchanged and must remain valid
// until C has stopped invoking `code`; session registries provide that
// lifetime and only move immutable callback contexts across their relay.
unsafe impl Send for FfiClosure {}

impl FfiClosure {
    /// Prepares a closure for `signature` and returns its C-callable address.
    ///
    /// The callback runs synchronously on the C caller's stack.
    ///
    /// # Safety
    /// The caller owns `user_data` and must keep it valid until this closure is
    /// dropped, which is also when C must have stopped invoking the returned
    /// code address.
    pub unsafe fn new(
        runtime: &LibffiRuntime,
        signature: &ForeignSignature,
        aggregates: &ForeignAggregates,
        callback: FfiClosureCallback,
        user_data: *mut c_void,
    ) -> Result<Self, LibffiError> {
        let prepared = Box::new(PreparedCif::new(&runtime.api, signature, aggregates)?);
        let mut code = std::ptr::null_mut();
        // SAFETY: the size is supplied by this libffi build and the returned
        // closure/code pointers are checked before being used.
        let size = unsafe { (runtime.api.closure_size)() };
        // SAFETY: libffi owns the executable closure allocation and returns its
        // code address through `code`; both are released by Drop.
        let closure = unsafe { (runtime.api.closure_alloc)(size, &mut code) };
        if closure.is_null() || code.is_null() {
            if !closure.is_null() {
                // SAFETY: `closure` came from this libffi allocation call and
                // the code address was unusable, so it is released immediately.
                unsafe { (runtime.api.closure_free)(closure) };
            }
            return Err(LibffiError::Storage {
                size,
                alignment: align_of::<usize>(),
            });
        }
        let mut result = Self {
            runtime: std::sync::Arc::clone(&runtime.api),
            prepared,
            closure,
            code,
            user_data,
        };
        let status = unsafe {
            // SAFETY: the closure storage, CIF, callback, and user data remain
            // alive for the whole lifetime of `result`.
            (result.runtime.prep_closure_loc)(
                result.closure,
                &mut result.prepared.cif,
                callback,
                user_data,
                result.code,
            )
        };
        if status != 0 {
            return Err(LibffiError::Prepare { status });
        }
        Ok(result)
    }

    /// The executable C function-pointer address.
    pub fn code(&self) -> *mut c_void {
        self.code
    }

    /// The caller-owned user-data pointer supplied at construction.
    pub fn user_data(&self) -> *mut c_void {
        self.user_data
    }

    /// The prepared CIF pointer visible to a diagnostic or test callback.
    pub fn cif(&self) -> *const RawFfiCif {
        &self.prepared.cif
    }
}

impl Drop for FfiClosure {
    fn drop(&mut self) {
        // SAFETY: the closure came from `ffi_closure_alloc`, has not been
        // released, and the libffi library remains alive in `runtime`.
        unsafe { (self.runtime.closure_free)(self.closure) };
    }
}
