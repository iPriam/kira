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

use std::ffi::{CStr, c_char};
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

/// Copies a Kira string into C storage that outlives the call, returning its
/// address; consumes the string handle.
///
/// The native mirror of the VM's `CStringNew`, and the same storage rule: see
/// [`kira_runtime_abi::c_storage`] for why a `CString` *member* of a struct C
/// keeps can never be freed on any schedule this side can guess.
///
/// # Safety
/// `value` must be null or a live handle from this runtime; it is freed here.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_rt_cstring_retain(value: KStr) -> i64 {
    // SAFETY: the caller vouches for the handle; `bytes_of` accepts null.
    let bytes = unsafe { bytes_of(value) };
    let word = match std::str::from_utf8(bytes) {
        Ok(text) => kira_runtime_abi::c_storage::retain_text(text),
        // Not UTF-8, so it is not a Kira `String` this runtime produced. A C
        // string of the raw bytes would be a value the program never wrote, so
        // this answers null exactly as an interior NUL does.
        Err(_) => 0,
    };
    // SAFETY: same handle, given up by the caller.
    unsafe { drop_handle(value) };
    word as i64
}

/// Copies `len` bytes of a C-layout image into storage that outlives the call,
/// returning its address.
///
/// The native mirror of the VM's `CLayoutAddress`. See
/// [`kira_runtime_abi::c_storage`] for why the image is never released.
///
/// # Safety
/// `src` must address at least `len` initialized bytes for this call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_rt_clayout_retain(src: *const u8, len: i64) -> i64 {
    if src.is_null() || len <= 0 {
        return 0;
    }
    // SAFETY: the caller guarantees `len` initialized bytes at `src`.
    let bytes = unsafe { slice::from_raw_parts(src, len as usize) };
    kira_runtime_abi::c_storage::retain_bytes(bytes) as i64
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

/// A string's length in bytes (`s.count`), freeing the string.
///
/// Bytes, not characters — the same units `charAt` and `substring` index, and
/// the same count the VM's own `StringLen` produces, which is what keeps the
/// two engines agreeing on text that is not all ASCII.
///
/// Consumes its argument, like every other operation that reads a string here.
///
/// Appended after the string helpers, so it is not an ABI change.
///
/// # Safety
/// `value` must be null or a live handle; it is freed here.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_rt_str_count(value: KStr) -> i64 {
    // SAFETY: caller passes a live (or null) handle that outlives this read.
    let counted = unsafe { bytes_of(value) }.len();
    // SAFETY: the same handle, consumed exactly once here.
    unsafe { drop_handle(value) };
    // A string longer than `i64::MAX` bytes cannot be built on any target this
    // runs on; saturating says so without a panic in an `extern "C"` frame.
    i64::try_from(counted).unwrap_or(i64::MAX)
}

/// The byte at `index` of a string (`s.charAt(i)`), freeing the string.
///
/// Traps on an index outside `0 ..< len` rather than answering, which is the
/// same trap the VM raises: a program that walks off the end of a string fails
/// identically on both engines instead of one of them inventing a byte.
///
/// # Safety
/// `value` must be null or a live handle; it is freed here.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_rt_str_char_at(value: KStr, index: i64) -> i64 {
    // SAFETY: caller passes a live (or null) handle that outlives this read.
    let read = usize::try_from(index)
        .ok()
        .and_then(|at| unsafe { bytes_of(value) }.get(at).copied());
    // SAFETY: the same handle, consumed exactly once here.
    unsafe { drop_handle(value) };
    match read {
        Some(byte) => i64::from(byte),
        None => kira_rt_trap_string_range(),
    }
}

/// A half-open byte slice of a string (`s.substring(start, end)`), freeing the
/// original and returning a fresh handle.
///
/// Traps when the range is inverted, out of bounds, or would split a multi-byte
/// character — the last because no Kira `String` can hold the result, so
/// answering would mean handing back bytes that are not text.
///
/// # Safety
/// `value` must be null or a live handle; it is freed here, and the result is a
/// fresh handle the caller owns.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_rt_str_substring(value: KStr, start: i64, end: i64) -> KStr {
    // SAFETY: caller passes a live (or null) handle that outlives this read.
    let bytes = unsafe { bytes_of(value) };
    let inverted = start > end;
    let carved = match (usize::try_from(start), usize::try_from(end)) {
        (Ok(from), Ok(to)) if from <= to => core::str::from_utf8(bytes)
            .ok()
            .and_then(|text| text.get(from..to))
            .map(str::to_owned),
        _ => None,
    };
    // SAFETY: the same handle, consumed exactly once here.
    unsafe { drop_handle(value) };
    match carved {
        Some(text) => bytes_to_handle(text.into_bytes()),
        None if inverted => kira_rt_trap_substring_inverted(),
        None => kira_rt_trap_string_range(),
    }
}

/// The byte index of the first occurrence of `needle` in `value`, or `-1`,
/// freeing both.
///
/// An empty needle matches at the front, so it answers `0` — the same answer
/// the VM's `StringIndexOf` gives.
///
/// # Safety
/// Both arguments must be null or live handles; both are freed here.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_rt_str_index_of(value: KStr, needle: KStr) -> i64 {
    // SAFETY: caller passes live (or null) handles that outlive these reads.
    let found = unsafe {
        let haystack = bytes_of(value);
        let pattern = bytes_of(needle);
        find_bytes(haystack, pattern)
    };
    // SAFETY: the same handles, each consumed exactly once here.
    unsafe {
        drop_handle(value);
        drop_handle(needle);
    }
    found.and_then(|at| i64::try_from(at).ok()).unwrap_or(-1)
}

/// Wraps owned bytes in a handle, using the runtime's one empty-string
/// representation for an empty result.
fn bytes_to_handle(bytes: Vec<u8>) -> KStr {
    if bytes.is_empty() {
        return std::ptr::null_mut();
    }
    into_handle(bytes.into_boxed_slice())
}

/// The index of the first occurrence of `pattern` in `haystack`.
fn find_bytes(haystack: &[u8], pattern: &[u8]) -> Option<usize> {
    if pattern.is_empty() {
        return Some(0);
    }
    if pattern.len() > haystack.len() {
        return None;
    }
    (0..=haystack.len() - pattern.len()).find(|&at| &haystack[at..at + pattern.len()] == pattern)
}

/// An `Int` rendered as a fresh string (`String(x)`).
///
/// The same spelling [`kira_rt_print_int`] gives, so a value printed and a
/// value converted never disagree.
#[unsafe(no_mangle)]
pub extern "C" fn kira_rt_str_of_int(value: i64) -> KStr {
    bytes_to_handle(value.to_string().into_bytes())
}

/// A `Float` rendered as a fresh string; see [`kira_rt_str_of_int`].
#[unsafe(no_mangle)]
pub extern "C" fn kira_rt_str_of_float(value: f64) -> KStr {
    bytes_to_handle(value.to_string().into_bytes())
}

/// A `Bool` rendered as a fresh string; see [`kira_rt_str_of_int`].
#[unsafe(no_mangle)]
pub extern "C" fn kira_rt_str_of_bool(value: u8) -> KStr {
    let text: &[u8] = if value != 0 { b"true" } else { b"false" };
    bytes_to_handle(text.to_vec())
}

/// Reports a string index out of bounds and exits with a failure code.
///
/// The native mirror of the VM's `StringIndexOutOfBounds`, worded identically:
/// the two engines have to fail the same way, and a trap that reads differently
/// on one backend is not the same trap.
#[unsafe(no_mangle)]
pub extern "C" fn kira_rt_trap_string_range() -> ! {
    eprintln!("kira: runtime trap: string index is out of bounds");
    std::process::exit(1);
}

/// Reports an inverted `substring` range and exits with a failure code.
///
/// The native mirror of the VM's `InvertedSubstring`. Kept distinct from
/// [`kira_rt_trap_string_range`] for the same reason the VM keeps its two
/// apart: a bound past the end and a start past its own end are different
/// mistakes, and one message for both would say less.
#[unsafe(no_mangle)]
pub extern "C" fn kira_rt_trap_substring_inverted() -> ! {
    eprintln!("kira: runtime trap: substring range is inverted");
    std::process::exit(1);
}

/// Copies a NUL-terminated C string into a fresh owned handle.
///
/// The native half of a `CString` **result**: a foreign function hands back a
/// `const char*` it keeps, and this copies the bytes while that pointer is still
/// good, so nothing in Kira ever holds C storage or is asked to free it. That
/// copy is the whole ownership answer, and it is the same one the VM's seam and
/// the hybrid host give — which is what makes the three print the same string.
///
/// A null pointer is the empty string: a C function returning `NULL` for
/// "nothing" is the one reading that invents no value.
///
/// Appended after the string helpers, so it is not an ABI change.
///
/// # Safety
/// `value` must be null or point at a NUL-terminated byte sequence that stays
/// valid for the length of this call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_rt_str_from_cstr(value: *const c_char) -> KStr {
    if value.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: caller guarantees a NUL-terminated sequence valid for this call.
    let bytes = unsafe { CStr::from_ptr(value) }.to_bytes();
    if bytes.is_empty() {
        return std::ptr::null_mut();
    }
    into_handle(bytes.to_vec().into_boxed_slice())
}

/// Reports a division-by-zero trap and exits with a failure code, mirroring the
/// VM's `DivideByZero` trap: no program output, non-zero exit.
#[unsafe(no_mangle)]
pub extern "C" fn kira_rt_trap_div_zero() -> ! {
    eprintln!("kira: runtime trap: division by zero");
    std::process::exit(1);
}

/// Reports a foreign-call trap and exits with a failure code.
///
/// The native mirror of the VM surfacing a [`kira_runtime_abi::ForeignCallError`]:
/// a generated adapter that returns a non-success status (an interior NUL in a
/// `CString`, say) has no value to hand back, so native code that called it
/// through the adapter reports the status and exits non-zero, exactly as
/// [`kira_rt_trap_div_zero`] does for division by zero. `status` is the
/// [`kira_runtime_abi::ForeignAdapterStatus`] word the adapter returned.
#[unsafe(no_mangle)]
pub extern "C" fn kira_rt_trap_foreign(status: u32) -> ! {
    eprintln!("kira: runtime trap: foreign call failed (adapter status {status})");
    std::process::exit(1);
}

/// Reports a Kira array that does not fit the inline C array it was crossing as,
/// and exits with a failure code.
///
/// A `@FFI.Array` member reserves `count` elements of C storage. A Kira array
/// holding more has elements with nowhere to go, and writing only the ones that
/// fit would hand C a different value than the program wrote — so both engines
/// trap here instead. The VM's `ForeignArrayTooLong` says the same thing in the
/// same words.
#[unsafe(no_mangle)]
pub extern "C" fn kira_rt_trap_foreign_array(count: u64, len: u64) -> ! {
    eprintln!(
        "kira: runtime trap: {len} elements do not fit the inline C array of {count} at the foreign seam"
    );
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
pub extern "C" fn kira_rt_abi_version_2() {}

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
            "kira_rt_abi_version_2"
        );
        assert_eq!(kira_runtime_abi::RUNTIME_ABI_VERSION, 2);
        // Referenced so the marker cannot be dead-code-eliminated out of an
        // rlib build, and so a rename breaks this test rather than the link.
        kira_rt_abi_version_2();
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
