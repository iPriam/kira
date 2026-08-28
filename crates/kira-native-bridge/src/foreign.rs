//! Foreign-adapter support: the version marker and the transient C-string
//! storage a generated adapter uses at the C boundary.
//!
//! A generated foreign adapter (emitted by the LLVM backend, one per
//! `@FFI.Extern` import) is `extern "C"` code that converts a Kira value to its
//! exact C type and calls the real C symbol. Two things it needs live here
//! rather than in generated code, because both are ordinary Rust that every
//! adapter shares:
//!
//! - [`kira_foreign_adapter_abi_version_3`], the versioned marker an adapter
//!   library exports and a loader checks by name — the foreign-seam mirror of
//!   [`crate::runtime::kira_rt_abi_version_11`]. A stale sidecar does not define
//!   this version's marker, so loading it fails by name instead of running the
//!   wrong ABI.
//! - [`kira_rt_cstring_new`]/[`kira_rt_cstring_free`], which turn a Kira
//!   `String` handle into a transient NUL-terminated C string for one call and
//!   free it afterwards. Interior NUL is reported as a null return so the
//!   adapter can raise the interior-NUL status rather than pass a truncated
//!   string.
//!
//! # Wire contract
//!
//! These symbols and signatures are shared with generated adapter code and with
//! the adapter loader, and are append-only: never rename one or change a
//! signature in place. They are additions to the `kira_rt_*` surface and do not
//! change [`kira_runtime_abi::RUNTIME_ABI_VERSION`].

use std::alloc::{Layout, alloc, dealloc};
use std::ffi::c_char;
use std::slice;

use crate::runtime::{KStr, kira_rt_str_data, kira_rt_str_len};

/// The versioned marker every generated foreign-adapter library exports.
///
/// Does nothing and costs nothing; its *name* is the compatibility check. An
/// adapter references it so the linker keeps it, and a loader resolves it by
/// name before binding any adapter. The name must always spell
/// [`kira_runtime_abi::FOREIGN_ADAPTER_ABI_MARKER`]; the test below keeps the
/// two from drifting.
#[unsafe(no_mangle)]
pub extern "C" fn kira_foreign_adapter_abi_version_3() {}

/// Allocates a transient NUL-terminated C string from a Kira `String` handle.
///
/// The returned pointer is valid until [`kira_rt_cstring_free`] releases it. A
/// null return means the string contained an interior NUL byte and cannot be a
/// C string; the adapter maps that to the interior-NUL status. The empty string
/// returns a valid pointer to a lone terminator, so null is unambiguous.
///
/// # Safety
/// `handle` must be null or a live [`KStr`] from this runtime; it is read, not
/// consumed. The returned pointer must be freed exactly once with
/// [`kira_rt_cstring_free`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_rt_cstring_new(handle: KStr) -> *mut c_char {
    // SAFETY: the caller guarantees `handle` is null or live; this reads it.
    let len = unsafe { kira_rt_str_len(handle) };
    // SAFETY: same contract as above; the pointer is valid for `len` bytes.
    let data = unsafe { kira_rt_str_data(handle) };
    let bytes = if len == 0 {
        &[][..]
    } else {
        // SAFETY: `kira_rt_str_data` returns a pointer valid for `len` bytes.
        unsafe { slice::from_raw_parts(data, len) }
    };
    if bytes.contains(&0) {
        // An interior NUL cannot be a C string; the adapter turns null into the
        // interior-NUL status rather than passing a truncated string.
        return std::ptr::null_mut();
    }
    let Ok(layout) = Layout::array::<u8>(len + 1) else {
        return std::ptr::null_mut();
    };
    // SAFETY: `layout` has non-zero size (`len + 1 >= 1`).
    let ptr = unsafe { alloc(layout) };
    if ptr.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: `ptr` addresses `len + 1` writable bytes just allocated; `bytes`
    // covers `len` readable bytes and does not overlap the fresh allocation.
    unsafe {
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr, len);
        ptr.add(len).write(0);
    }
    ptr.cast()
}

/// Frees a transient C string from [`kira_rt_cstring_new`]. Null is a no-op.
///
/// # Safety
/// `ptr` must be null or a pointer returned by [`kira_rt_cstring_new`] and not
/// yet freed. It is freed here.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_rt_cstring_free(ptr: *mut c_char) {
    if ptr.is_null() {
        return;
    }
    // The string has no interior NUL by construction, so its length is where
    // the terminator sits — the same length `kira_rt_cstring_new` allocated for.
    let mut len = 0usize;
    // SAFETY: `ptr` points at a NUL-terminated buffer this module allocated.
    while unsafe { *ptr.add(len) } != 0 {
        len += 1;
    }
    let Ok(layout) = Layout::array::<u8>(len + 1) else {
        return;
    };
    // SAFETY: `ptr` came from `alloc` with exactly this layout in
    // `kira_rt_cstring_new`, and the caller frees it at most once.
    unsafe { dealloc(ptr.cast(), layout) };
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::{kira_rt_str_free, kira_rt_str_new};

    fn handle(text: &str) -> KStr {
        // SAFETY: the slice covers exactly `len` readable bytes.
        unsafe { kira_rt_str_new(text.as_ptr(), text.len()) }
    }

    #[test]
    fn the_marker_matches_the_shared_contract() {
        assert_eq!(
            kira_runtime_abi::FOREIGN_ADAPTER_ABI_MARKER,
            "kira_foreign_adapter_abi_version_3"
        );
        // Referenced so a rename breaks this test rather than a link.
        kira_foreign_adapter_abi_version_3();
    }

    #[test]
    fn a_c_string_is_nul_terminated_and_freeable() {
        let source = handle("kira");
        // SAFETY: `source` is a live handle for this call.
        let c = unsafe { kira_rt_cstring_new(source) };
        assert!(!c.is_null());
        // SAFETY: the buffer holds "kira\0".
        unsafe {
            assert_eq!(*c.add(0) as u8, b'k');
            assert_eq!(*c.add(4) as u8, 0);
            kira_rt_cstring_free(c);
            kira_rt_str_free(source);
        }
    }

    #[test]
    fn the_empty_string_is_a_lone_terminator_not_null() {
        let source = handle("");
        // SAFETY: `source` is a null (empty) handle, which is valid here.
        let c = unsafe { kira_rt_cstring_new(source) };
        assert!(!c.is_null(), "empty is a valid C string, not interior NUL");
        // SAFETY: the buffer holds a single terminator.
        unsafe {
            assert_eq!(*c as u8, 0);
            kira_rt_cstring_free(c);
        }
    }

    #[test]
    fn an_interior_nul_is_reported_as_null() {
        let source = handle("a\0b");
        // SAFETY: `source` is a live handle whose bytes contain an interior NUL.
        let c = unsafe { kira_rt_cstring_new(source) };
        assert!(c.is_null());
        // SAFETY: freeing null is a no-op; the source handle is freed once.
        unsafe {
            kira_rt_cstring_free(c);
            kira_rt_str_free(source);
        }
    }

    #[test]
    fn freeing_null_is_a_no_op() {
        // SAFETY: null is the documented no-op input.
        unsafe { kira_rt_cstring_free(std::ptr::null_mut()) };
    }
}
