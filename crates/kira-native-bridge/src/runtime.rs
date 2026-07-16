//! The native runtime support library: stable C-ABI helpers for `print` and
//! Kira `String` values, linked into every LLVM-native executable.
//!
//! # ABI
//!
//! A Kira `String` crosses the boundary as a [`KStr`]: an *opaque owned handle*
//! — one pointer, never an aggregate. This mirrors the VM, where a string value
//! is a handle into the string heap rather than the bytes themselves, and it
//! keeps the ABI trivially correct: LLVM IR is not ABI-aware (aggregate passing
//! is a frontend's job), so a backend that only ever passes and returns
//! pointer-sized scalars cannot disagree with this crate's `extern "C"` about
//! how a value is laid out in registers.
//!
//! # Ownership
//!
//! Ownership is affine, mirroring the VM's string heap: reading a local *clones*
//! ([`kira_rt_str_clone`]); every operation that consumes a string *frees* it
//! ([`kira_rt_str_concat`], [`kira_rt_str_eq`], [`kira_rt_print_str`]);
//! reassigning or leaving a local frees the old value ([`kira_rt_str_free`]). A
//! well-formed program frees every allocation exactly once, so nothing leaks —
//! the same guarantee the VM proves with heap accounting.
//!
//! Because these helpers format with the same standard library the VM uses,
//! `print` output is identical byte-for-byte across `kira run` (VM) and a native
//! executable.
//!
//! Every symbol is `extern "C"` with a `kira_rt_` prefix and a fixed signature.
//! These names are a wire contract with the backend's lowering and are
//! append-only: never rename one or change a signature in place.

use std::io::Write;
use std::slice;

/// A Kira `String` at the native ABI: an opaque owned handle.
///
/// The backend treats this as a pointer-sized scalar and never inspects it. A
/// null handle is the empty string, so a zero-initialized local slot is already
/// a valid (and free-able) value — the native mirror of the VM initializing
/// every slot to `Void`.
pub type KStr = *mut KiraString;

/// The heap object behind a [`KStr`]: an owned, immutable UTF-8 buffer.
///
/// Kira strings have value semantics, so this is never shared or mutated: every
/// operation that would observe a change produces a fresh allocation instead.
pub struct KiraString {
    bytes: Box<[u8]>,
}

/// Boxes `bytes` into a fresh handle.
fn into_handle(bytes: Box<[u8]>) -> KStr {
    Box::into_raw(Box::new(KiraString { bytes }))
}

/// Borrows a handle's bytes; a null handle is the empty string.
///
/// # Safety
/// `handle` must be null or a live handle from this runtime.
unsafe fn bytes_of<'a>(handle: KStr) -> &'a [u8] {
    if handle.is_null() {
        return &[];
    }
    // SAFETY: a non-null handle is a live `KiraString` the caller has not freed,
    // and this runtime never hands out a mutable alias to it.
    unsafe { &(*handle).bytes }
}

/// Takes ownership of a handle and drops it; a null handle is a no-op.
///
/// # Safety
/// `handle` must be null or a live handle from this runtime, freed at most once.
unsafe fn drop_handle(handle: KStr) {
    if handle.is_null() {
        return;
    }
    // SAFETY: the handle came from `Box::into_raw` in `into_handle`, and the
    // caller's free-once contract makes this the only reclaim of it.
    drop(unsafe { Box::from_raw(handle) });
}

/// Writes `bytes` followed by a newline to stdout, matching the VM host's
/// line-oriented `print`.
fn print_line(bytes: &[u8]) {
    let stdout = std::io::stdout();
    let mut handle = stdout.lock();
    // Best-effort, like the VM host's `println!`: a closed pipe is not a
    // Kira-observable condition, so a failed write is dropped rather than
    // turned into a trap.
    let _ = handle.write_all(bytes);
    let _ = handle.write_all(b"\n");
}

/// Prints an `Int` and a newline. Mirrors the VM's `Int` formatting.
#[unsafe(no_mangle)]
pub extern "C" fn kira_rt_print_int(value: i64) {
    print_line(value.to_string().as_bytes());
}

/// Prints a `Float` and a newline.
///
/// Uses the standard library's `f64` `Display`, exactly as the VM does, so a
/// whole float prints without a decimal point (`2.0` -> `2`).
#[unsafe(no_mangle)]
pub extern "C" fn kira_rt_print_float(value: f64) {
    print_line(value.to_string().as_bytes());
}

/// Prints a `Bool` and a newline (`true`/`false`).
#[unsafe(no_mangle)]
pub extern "C" fn kira_rt_print_bool(value: u8) {
    print_line(if value != 0 { b"true" } else { b"false" });
}

/// Prints a `String` and a newline, then frees it (`print` consumes its value).
///
/// # Safety
/// `value` must be null or a live handle from this runtime; it is freed here.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_rt_print_str(value: KStr) {
    // SAFETY: caller passes a live (or null) handle.
    print_line(unsafe { bytes_of(value) });
    // SAFETY: the same handle, consumed exactly once here.
    unsafe { drop_handle(value) };
}

/// Materializes a `String` literal: copies `len` bytes from `data` into a fresh
/// owned handle.
///
/// # Safety
/// `data` must point at `len` readable bytes, or be null when `len == 0`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_rt_str_new(data: *const u8, len: usize) -> KStr {
    if len == 0 {
        return std::ptr::null_mut();
    }
    // SAFETY: caller guarantees `data` covers `len` readable bytes.
    let bytes = unsafe { slice::from_raw_parts(data, len) };
    into_handle(bytes.to_vec().into_boxed_slice())
}

/// Produces an independent copy of a string (clone-on-read for locals).
///
/// # Safety
/// `value` must be null or a live handle; it is left untouched.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_rt_str_clone(value: KStr) -> KStr {
    // SAFETY: caller passes a live (or null) handle that outlives this read.
    let bytes = unsafe { bytes_of(value) };
    if bytes.is_empty() {
        return std::ptr::null_mut();
    }
    into_handle(bytes.to_vec().into_boxed_slice())
}

/// Concatenates two strings into a fresh one, freeing both inputs.
///
/// # Safety
/// `a` and `b` must each be null or a live handle; both are freed here.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_rt_str_concat(a: KStr, b: KStr) -> KStr {
    // SAFETY: caller passes live (or null) handles.
    let (left, right) = unsafe { (bytes_of(a), bytes_of(b)) };
    let mut joined = Vec::with_capacity(left.len() + right.len());
    joined.extend_from_slice(left);
    joined.extend_from_slice(right);
    let result = if joined.is_empty() {
        std::ptr::null_mut()
    } else {
        into_handle(joined.into_boxed_slice())
    };
    // SAFETY: both inputs are live and consumed exactly once here.
    unsafe {
        drop_handle(a);
        drop_handle(b);
    }
    result
}

/// Compares two strings for equality, freeing both inputs. Returns 0 or 1.
///
/// # Safety
/// `a` and `b` must each be null or a live handle; both are freed here.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_rt_str_eq(a: KStr, b: KStr) -> u8 {
    // SAFETY: caller passes live (or null) handles.
    let equal = unsafe { bytes_of(a) == bytes_of(b) };
    // SAFETY: both inputs are live and consumed exactly once here.
    unsafe {
        drop_handle(a);
        drop_handle(b);
    }
    u8::from(equal)
}

/// Frees a string. A null handle is a no-op.
///
/// # Safety
/// `value` must be null or a live handle from this runtime, freed at most once.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_rt_str_free(value: KStr) {
    // SAFETY: caller upholds the free-once contract.
    unsafe { drop_handle(value) };
}

/// Borrows a string's bytes; null for the empty string.
///
/// Generated code never calls this — a [`KStr`] is opaque to the backend, which
/// only ever moves the handle around. The *host* of a hybrid program needs it:
/// a string handed across the boundary is a handle into this runtime's heap, and
/// a handle is exactly what a reader outside this crate cannot dereference. This
/// and [`kira_rt_str_len`] are that reader's only way in.
///
/// The bytes are borrowed, valid until the handle is freed, and always UTF-8:
/// every handle this runtime builds comes from Kira `String` data.
///
/// # Safety
/// `value` must be null or a live handle; the returned pointer is valid for
/// [`kira_rt_str_len`] bytes and only until `value` is freed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_rt_str_data(value: KStr) -> *const u8 {
    // SAFETY: caller passes a live (or null) handle that outlives this read.
    unsafe { bytes_of(value) }.as_ptr()
}

/// The length in bytes of a string; 0 for the empty (or null) string.
///
/// The companion of [`kira_rt_str_data`]; see it for why both exist.
///
/// # Safety
/// `value` must be null or a live handle from this runtime.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_rt_str_len(value: KStr) -> usize {
    // SAFETY: caller passes a live (or null) handle that outlives this read.
    unsafe { bytes_of(value) }.len()
}

/// Reports a division-by-zero trap and exits with a failure code, mirroring the
/// VM's `DivideByZero` trap: no program output, non-zero exit.
#[unsafe(no_mangle)]
pub extern "C" fn kira_rt_trap_div_zero() -> ! {
    eprintln!("kira: runtime trap: division by zero");
    std::process::exit(1);
}

/// The ABI marker for this archive's version of the `kira_rt_*` contract.
///
/// Does nothing and costs nothing; its *name* is the point. Generated code
/// references it, so an archive built against a different version of the
/// contract fails to link — by name, at build time — instead of resolving the
/// old code under the new ABI and corrupting memory at run time.
///
/// The name must always spell [`kira_runtime_abi::RUNTIME_ABI_MARKER`]; the test
/// below is what keeps the two from drifting.
#[unsafe(no_mangle)]
pub extern "C" fn kira_rt_abi_version_1() {}

#[cfg(test)]
mod tests {
    use super::*;

    /// The marker this archive defines must be the one generated code
    /// references, or the guard silently guards nothing: the backend would emit
    /// a reference to a symbol no archive ever defines (every link fails), or
    /// this archive would define a marker nobody checks (stale archives link
    /// again). Bumping `RUNTIME_ABI_VERSION` means renaming the function above.
    #[test]
    fn the_abi_marker_matches_the_shared_contract() {
        assert_eq!(
            kira_runtime_abi::RUNTIME_ABI_MARKER,
            "kira_rt_abi_version_1"
        );
        assert_eq!(kira_runtime_abi::RUNTIME_ABI_VERSION, 1);
        // Referenced so the marker cannot be dead-code-eliminated out of an
        // rlib build, and so a rename breaks this test rather than the link.
        kira_rt_abi_version_1();
    }

    /// Builds a handle from a literal, as the backend's lowering would.
    fn new(text: &str) -> KStr {
        // SAFETY: the slice covers exactly `len` readable bytes.
        unsafe { kira_rt_str_new(text.as_ptr(), text.len()) }
    }

    #[test]
    fn concat_clone_and_eq_follow_value_semantics() {
        // SAFETY: every handle below is live and consumed exactly once.
        unsafe {
            let joined =
                kira_rt_str_concat(kira_rt_str_concat(new("hello"), new(", ")), new("world"));
            assert_eq!(bytes_of(joined), b"hello, world");

            let copy = kira_rt_str_clone(joined);
            assert_eq!(bytes_of(copy), bytes_of(joined));
            assert_ne!(copy, joined, "a clone is an independent allocation");

            assert_eq!(kira_rt_str_eq(joined, copy), 1); // frees both
        }
    }

    #[test]
    fn distinct_contents_compare_unequal() {
        // SAFETY: both handles are live and consumed by the comparison.
        unsafe {
            assert_eq!(kira_rt_str_eq(new("hello"), new("kira")), 0);
        }
    }

    /// The host's only way to read a handle: it cannot dereference `KiraString`,
    /// which is this crate's private type.
    #[test]
    fn a_handles_bytes_are_readable_from_outside_this_crate() {
        // SAFETY: `handle` is live for both reads and freed exactly once.
        unsafe {
            let handle = new("hello, world");
            let data = kira_rt_str_data(handle);
            let len = kira_rt_str_len(handle);
            assert_eq!(slice::from_raw_parts(data, len), b"hello, world");
            kira_rt_str_free(handle);
        }
    }

    #[test]
    fn the_null_handle_is_the_empty_string() {
        // SAFETY: a null handle is a valid empty string; free is a no-op.
        unsafe {
            let empty: KStr = std::ptr::null_mut();
            assert_eq!(bytes_of(empty), b"");
            assert!(kira_rt_str_clone(empty).is_null());
            assert_eq!(kira_rt_str_eq(empty, new("")), 1);
            kira_rt_str_free(empty);
        }
    }
}
