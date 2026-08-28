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

/// The heap object behind a [`KStr`]: an immutable UTF-8 buffer and a count of
/// the values holding it.
///
/// Kira strings have value semantics and this object is never *mutated* — every
/// operation that would observe a change produces a fresh allocation instead —
/// which is exactly what lets it be **shared**. A copy adds a share and hands
/// back the same handle; there is no make-unique to do, because no writer
/// exists. See [`kira_rt_str_clone`].
///
/// `#[repr(C)]` because the backend reads `shares` directly rather than calling
/// for it: copying a string was a quarter of a Project Matter frame, and the
/// copy was an allocation plus a `memcpy` per read. `the_string_layout_is_pinned`
/// holds the layout, and [`kira_runtime_abi::STRING_SHARES_FIELD`] is the index
/// the backend GEPs with.
#[repr(C)]
pub struct KiraString {
    /// The bytes, owned by this object and never written after construction.
    bytes: Box<[u8]>,
    /// How many values hold this object; the bytes go with the last.
    shares: usize,
}

/// Boxes `bytes` into a fresh handle held by one value.
fn into_handle(bytes: Box<[u8]>) -> KStr {
    crate::accounting::record_alloc();
    Box::into_raw(Box::new(KiraString { bytes, shares: 1 }))
}

/// Borrows a handle's bytes; a null handle is the empty string.
///
/// # Safety
/// `handle` must be null or a live handle from this runtime.
pub(crate) unsafe fn bytes_of<'a>(handle: KStr) -> &'a [u8] {
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
pub(crate) unsafe fn drop_handle(handle: KStr) {
    if handle.is_null() {
        return;
    }
    // SAFETY: a non-null handle is a live `KiraString`.
    let shares = unsafe { &mut (*handle).shares };
    if *shares > 1 {
        // Another value still reads these bytes.
        *shares -= 1;
        return;
    }
    crate::accounting::record_free();
    // SAFETY: the handle came from `Box::into_raw` in `into_handle`, this was
    // the last hold on it, and the caller's release-once contract makes this
    // the only reclaim of it.
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

/// Produces a copy of a string: the same bytes, held once more.
///
/// A `KiraString` is never written after it is built, so two values holding one
/// are indistinguishable from two copies — there is no writer for a
/// copy-on-write scheme to protect against, and nothing to make unique. What it
/// removes is an allocation and a `memcpy` of the bytes on every read of every
/// string, which was a quarter of a Project Matter frame.
///
/// The empty string is the null handle and is not an object, so a copy of one
/// is itself.
///
/// The backend emits this inline and does not call it; it stays exported
/// because the name is part of the runtime's wire contract.
///
/// # Safety
/// `value` must be null or a live handle; it is left untouched but for its
/// share count.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_rt_str_clone(value: KStr) -> KStr {
    if value.is_null() {
        return value;
    }
    // SAFETY: a non-null handle is a live `KiraString`. The count cannot wrap:
    // it rises by one per live value holding it.
    unsafe { (*value).shares += 1 };
    value
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
pub(crate) fn bytes_to_handle(bytes: Vec<u8>) -> KStr {
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
    // `str::find` is a two-way search over a memchr-accelerated scan, which the
    // quadratic slide below is not: measured 26x apart on a megabyte of text.
    // The VM's `StringIndexOf` has always taken that path, so this is the two
    // engines agreeing on cost as well as on the answer.
    //
    // Both handles come from Kira `String`s, so they are UTF-8 and the fast
    // path is the only one taken in practice; the slide is what a corrupted
    // handle falls back to rather than a wrong answer.
    if let (Ok(text), Ok(needle)) = (
        core::str::from_utf8(haystack),
        core::str::from_utf8(pattern),
    ) {
        return text.find(needle);
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
    eprintln!(
        "kira: runtime trap: foreign call failed — {}",
        explain_foreign_status(status)
    );
    std::process::exit(1);
}

/// Reports a call into a library this platform does not have, and exits.
///
/// Named rather than numbered: knowing a call failed is useless next to knowing
/// which binding it was, and the adapter is the only place that knows.
///
/// # Safety
/// `library` and `symbol` must each be a pointer to a NUL-terminated C string
/// that stays readable for the duration of the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_rt_trap_foreign_unavailable(
    library: *const c_char,
    symbol: *const c_char,
) -> ! {
    // SAFETY: the caller guarantees both pointers address readable
    // NUL-terminated strings for the duration of this call.
    let name = |pointer: *const c_char| unsafe {
        if pointer.is_null() {
            "<unnamed>".to_owned()
        } else {
            CStr::from_ptr(pointer).to_string_lossy().into_owned()
        }
    };
    eprintln!(
        "kira: runtime trap: `{}` from native library `{}` was called, but that library is not \
         available on this platform and its declaration said it need not be",
        name(symbol),
        name(library)
    );
    print_trap_backtrace();
    std::process::exit(1);
}

/// The environment variable that asks a trap to print where it came from.
pub const TRAP_BACKTRACE_VAR: &str = "KIRA_TRAP_BACKTRACE";

/// Prints a backtrace when [`TRAP_BACKTRACE_VAR`] asks for one.
///
/// Off by default: a trap's message names what went wrong, and a wall of
/// frames after it would bury that. On demand it is the fastest way to find
/// which call reached a trap in a program with many.
///
/// This is how a trap whose *message* cannot name the offender still says
/// where it came from. An array trap names neither the index nor the length,
/// on purpose — a wasm trap path cannot format either without allocating
/// mid-trap, and a trap that reads differently on one backend is not the same
/// trap. The backtrace carries that detail out of band instead, where asking
/// for it is the reader's choice and parity is untouched.
pub(crate) fn print_trap_backtrace() {
    if std::env::var_os(TRAP_BACKTRACE_VAR).is_none() {
        return;
    }
    eprintln!("{}", std::backtrace::Backtrace::force_capture());
}

/// What a foreign-adapter status word means, in a sentence.
///
/// A bare number tells whoever hit it nothing. An unknown one is reported as
/// itself rather than guessed at, because a newer adapter library may return a
/// status this runtime has never heard of.
fn explain_foreign_status(status: u32) -> String {
    use kira_runtime_abi::ForeignAdapterStatus as Status;
    let known = match Status(status) {
        Status::SUCCESS => "the call succeeded, which is not a trap",
        Status::BAD_ARGUMENT_COUNT => {
            "the adapter was given a different argument count than its signature declares"
        }
        Status::BAD_ARGUMENT_TAG => "an argument arrived carrying the wrong bridge tag",
        Status::INTERIOR_NUL => {
            "a `CString` argument contained an interior NUL byte, so it cannot cross as a C string"
        }
        Status::MALFORMED_RESULT => "the adapter could not encode a valid result",
        Status::BAD_RESULT_SLOT => {
            "the caller presented no writable buffer for an aggregate result"
        }
        Status::UNAVAILABLE_LIBRARY => {
            "this import's native library is not available on this platform, and its declaration \
             said it need not be — so the call was never linked. Reaching it means code meant for \
             another platform ran here"
        }
        _ => {
            return format!(
                "the adapter returned status {status}, which this runtime does not know"
            );
        }
    };
    known.to_owned()
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
pub extern "C" fn kira_rt_abi_version_11() {}

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
            "kira_rt_abi_version_11"
        );
        assert_eq!(kira_runtime_abi::RUNTIME_ABI_VERSION, 11);
        // Referenced so the marker cannot be dead-code-eliminated out of an
        // rlib build, and so a rename breaks this test rather than the link.
        kira_rt_abi_version_11();
    }

    /// The backend reads `shares` out of this object, so its shape is a
    /// contract with code compiled separately from this crate. The `Box<[u8]>`
    /// in front of it is a fat pointer — two words — which is what puts the
    /// count at field index two.
    #[test]
    fn the_string_layout_is_pinned() {
        assert_eq!(size_of::<Box<[u8]>>(), 2 * size_of::<usize>());
        assert_eq!(size_of::<KiraString>(), 3 * size_of::<usize>());
        assert_eq!(align_of::<KiraString>(), align_of::<usize>());
        let owned = KiraString {
            bytes: Box::new([]),
            shares: 1,
        };
        let base = std::ptr::from_ref(&owned).cast::<u8>();
        // SAFETY: both fields belong to `owned`, which outlives the reads.
        unsafe {
            assert_eq!(
                std::ptr::from_ref(&owned.bytes)
                    .cast::<u8>()
                    .offset_from(base),
                0
            );
            assert_eq!(
                std::ptr::from_ref(&owned.shares)
                    .cast::<u8>()
                    .offset_from(base),
                isize::try_from(kira_runtime_abi::STRING_SHARES_FIELD).expect("a small index")
                    * size_of::<usize>() as isize
            );
        }
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
            // A copy is the same object, held twice: the bytes are never
            // written, so nothing can tell the two apart.
            assert_eq!(copy, joined, "a copy allocates nothing");
            assert_eq!((*joined).shares, 2);

            assert_eq!(kira_rt_str_eq(joined, copy), 1); // releases both
        }
    }

    /// The bytes go with the last value holding them, never with the first —
    /// which under Miri or ASan is the difference between a live read and a
    /// use-after-free.
    #[test]
    fn a_shared_string_outlives_every_hold_but_the_last() {
        // SAFETY: the handle is live and released once per hold.
        unsafe {
            let text = new("payload");
            let copy = kira_rt_str_clone(text);
            kira_rt_str_free(text);
            assert_eq!(bytes_of(copy), b"payload", "the bytes survived one hold");
            kira_rt_str_free(copy);
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
