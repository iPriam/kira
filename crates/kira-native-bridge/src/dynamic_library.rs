//! Opening a shared library by name and resolving symbols out of it.
//!
//! A graphics driver is not a build dependency. `vulkan-1.dll` and `d3d12.dll`
//! may simply be absent — an integrated GPU with no Vulkan runtime installed, a
//! Windows install without the D3D12 SDK layer — and a program that linked
//! against them would fail to start on that machine instead of falling back to
//! another backend. So the driver is opened on first use, and a machine without
//! it gets a null handle and an honest answer rather than a load error.
//!
//! That is the same reason `package.kira` declares these libraries
//! `LinkMode.Runtime` with `Availability.Optional`. This module is the runtime
//! half of that declaration.
//!
//! Symbols come back as bare addresses. What a caller does with one is call it
//! through [`crate::dynamic_call`], which is where the signature — unknown to
//! the compiler for a driver entry point resolved by name — is supplied.
//!
//! # Ownership
//!
//! A handle owns the loaded library and unloads it when
//! [`kira_dynamic_library_close`] is called. Every symbol resolved from it is a
//! bare pointer with no lifetime attached, so calling one after the close is a
//! use-after-unload the caller has to avoid — the same contract `dlsym` has.

use kira_dynamic_ffi::DynamicLibrary;
use std::ffi::{CStr, c_char, c_void};

/// Opens the shared library named `name`, or returns null when it cannot.
///
/// Null covers every reason equally — absent, unreadable, wrong architecture,
/// a name that is not UTF-8 — because a caller's next move is the same for all
/// of them: report the backend as unavailable and pick another. The distinction
/// a caller actually acts on is present versus absent.
///
/// # Safety
/// `name` must be null, or a pointer to a NUL-terminated C string that stays
/// valid for this call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_dynamic_library_open(name: *const c_char) -> *mut c_void {
    if name.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: the caller guarantees `name` is NUL-terminated and valid for the
    // duration of this call.
    let name = unsafe { CStr::from_ptr(name) };
    let Ok(name) = name.to_str() else {
        return std::ptr::null_mut();
    };
    let Ok(library) = DynamicLibrary::open(std::path::Path::new(name)) else {
        return std::ptr::null_mut();
    };
    Box::into_raw(Box::new(library)).cast::<c_void>()
}

/// Resolves `name` in `library`, or returns null when it is absent.
///
/// A null answer is the useful one for an optional entry point: a driver that
/// predates an extension exports every other symbol, and the caller degrades
/// rather than failing to load.
///
/// # Safety
/// `library` must be null, or a pointer from [`kira_dynamic_library_open`] that
/// has not been closed. `name` must be null, or a NUL-terminated C string valid
/// for this call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_dynamic_library_symbol(
    library: *mut c_void,
    name: *const c_char,
) -> *mut c_void {
    if library.is_null() || name.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: the caller guarantees `name` is NUL-terminated and valid here.
    let name = unsafe { CStr::from_ptr(name) };
    let Ok(name) = name.to_str() else {
        return std::ptr::null_mut();
    };
    // SAFETY: the caller guarantees `library` came from
    // `kira_dynamic_library_open` and has not been closed, so it addresses a
    // live `DynamicLibrary`.
    let library = unsafe { &*library.cast::<DynamicLibrary>() };
    // SAFETY: the symbol is taken only for its address, which is read back out
    // below without ever being called through this type. The real signature is
    // supplied at the call site in `dynamic_call`, which is where the caller
    // states it.
    let Some(symbol) = (unsafe { library.lookup_optional::<unsafe extern "C" fn()>(name) }) else {
        return std::ptr::null_mut();
    };
    (*symbol) as usize as *mut c_void
}

/// Closes a library from [`kira_dynamic_library_open`], unloading it.
///
/// A null pointer closes nothing, matching the shape of every other free here.
///
/// # Safety
/// `library` must be null, or a pointer from [`kira_dynamic_library_open`] that
/// has not already been closed. No symbol resolved from it may be called
/// afterwards.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_dynamic_library_close(library: *mut c_void) {
    if library.is_null() {
        return;
    }
    // SAFETY: the caller guarantees `library` came from
    // `kira_dynamic_library_open` and has not been closed, so reclaiming the
    // `Box` is sound and happens once.
    drop(unsafe { Box::from_raw(library.cast::<DynamicLibrary>()) });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The C library under whatever name this platform gives it, which every
    /// host running these tests already has loaded.
    const HOST_C_LIBRARY: &CStr = if cfg!(windows) {
        c"msvcrt.dll"
    } else if cfg!(target_vendor = "apple") {
        c"libSystem.B.dylib"
    } else {
        c"libc.so.6"
    };

    #[test]
    fn a_missing_library_is_a_null_handle_not_a_failure_to_start() {
        // SAFETY: the name is a valid NUL-terminated string.
        let handle =
            unsafe { kira_dynamic_library_open(c"kira-definitely-no-such-library".as_ptr()) };
        assert!(
            handle.is_null(),
            "an absent driver is the case this whole module exists for"
        );
    }

    #[test]
    fn opening_null_answers_null_rather_than_dereferencing_it() {
        // SAFETY: null is the one pointer the contract explicitly allows.
        assert!(unsafe { kira_dynamic_library_open(std::ptr::null()) }.is_null());
    }

    #[test]
    fn a_symbol_resolves_to_an_address_that_can_be_called() {
        // SAFETY: the name is valid, and the handle is closed once below.
        unsafe {
            let library = kira_dynamic_library_open(HOST_C_LIBRARY.as_ptr());
            assert!(!library.is_null(), "the host C library must be loadable");

            let absent = kira_dynamic_library_symbol(library, c"kira_no_such_symbol".as_ptr());
            assert!(absent.is_null(), "a missing symbol is null, not a fault");

            let abs = kira_dynamic_library_symbol(library, c"abs".as_ptr());
            assert!(!abs.is_null(), "`abs` is exported by every C library");

            // The address is real: call it through the builder and check it
            // computes, rather than trusting that non-null means correct.
            let call = crate::dynamic_call::kira_dynamic_call_new(1);
            crate::dynamic_call::kira_dynamic_call_arg_i32(call, -7);
            let result = crate::dynamic_call::kira_dynamic_call_invoke_i32(call, abs);
            assert_eq!(result, 7);
            crate::dynamic_call::kira_dynamic_call_free(call);

            kira_dynamic_library_close(library);
        }
    }

    #[test]
    fn resolving_through_a_null_library_answers_null() {
        // SAFETY: null is handled before any dereference.
        unsafe {
            assert!(kira_dynamic_library_symbol(std::ptr::null_mut(), c"abs".as_ptr()).is_null());
            kira_dynamic_library_close(std::ptr::null_mut());
        }
    }
}
