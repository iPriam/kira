//! Reading and building the runtime's string and array values from Rust.
//!
//! The glue every `kira_rt_*` helper that speaks in Kira values needs: read a
//! string handle as text, box text back into one, and read or build a
//! `[String]`. It is shared rather than repeated because a second copy of it is
//! a second set of ownership rules, and the rules are what make these helpers
//! correct.
//!
//! # Ownership
//!
//! Affine, like the rest of this runtime: a helper **consumes the handles it is
//! given**, and every value it builds is owned by whoever it is handed to. The
//! functions here do not decide that — they are the mechanics a helper uses to
//! carry it out.

use crate::array::{KArray, kira_rt_array_free, kira_rt_array_len, kira_rt_array_new};
use crate::runtime::{KStr, kira_rt_str_data, kira_rt_str_free, kira_rt_str_len, kira_rt_str_new};

/// Reads one string handle's bytes as text, without taking ownership.
///
/// Lossy on invalid UTF-8, exactly as the VM is when it reads the same value out
/// of its heap: a `String` there is Rust-owned text, so both engines round the
/// same non-UTF-8 bytes the same way.
///
/// # Safety
/// `handle` must be null or a live handle from this runtime.
#[must_use]
pub unsafe fn text_of(handle: KStr) -> String {
    // SAFETY: the caller vouches for the handle; both accessors accept null.
    let (data, len) = unsafe { (kira_rt_str_data(handle), kira_rt_str_len(handle)) };
    if data.is_null() || len == 0 {
        return String::new();
    }
    // SAFETY: `data` addresses `len` initialized bytes owned by the handle,
    // which stays live for this borrow.
    let bytes = unsafe { std::slice::from_raw_parts(data, len) };
    String::from_utf8_lossy(bytes).into_owned()
}

/// Frees a string handle.
///
/// # Safety
/// `handle` must be null or a live handle from this runtime, freed at most once.
pub unsafe fn release(handle: KStr) {
    // SAFETY: forwarded contract.
    unsafe { kira_rt_str_free(handle) };
}

/// Boxes text into a fresh string handle.
#[must_use]
pub fn handle_of(text: &str) -> KStr {
    // SAFETY: the pointer and length describe one live UTF-8 buffer.
    unsafe { kira_rt_str_new(text.as_ptr(), text.len()) }
}

/// Builds a Kira array whose elements are eight-byte integers.
///
/// `esize` is the element stride the backend computed from the target's ABI. A
/// Kira integer is an `i64` in generated code, so the value written into each
/// slot is an `i64`; `esize` decides where the next slot starts.
///
/// # Safety
/// `esize` must be the ABI size the backend gives a Kira integer element.
#[must_use]
pub unsafe fn int_array(values: &[i64], esize: i64) -> KArray {
    let stride = esize.max(0) as usize;
    let array = kira_rt_array_new(values.len(), stride);
    // SAFETY: the array was just built with exactly `values.len()` slots, and
    // `stride` is the size it was built with.
    unsafe {
        write_slots(array, values.len(), stride, |slot, index| {
            slot.cast::<i64>().write(values[index])
        })
    };
    array
}

/// Builds a Kira array of string handles, one fresh handle per element.
///
/// # Safety
/// `esize` must be the ABI size the backend gives a string-handle element.
#[must_use]
pub unsafe fn string_array(texts: &[String], esize: i64) -> KArray {
    let stride = esize.max(0) as usize;
    let array = kira_rt_array_new(texts.len(), stride);
    // SAFETY: as in `int_array`; each slot receives one owned handle, which the
    // array's element free leaf releases.
    unsafe {
        write_slots(array, texts.len(), stride, |slot, index| {
            slot.cast::<KStr>().write(handle_of(&texts[index]));
        });
    }
    array
}

/// Reads a Kira `[String]` as owned text, then frees the array and its handles.
///
/// The reading and the freeing are one operation because the caller is a helper
/// that consumes what it is given: separating them is how a helper that returns
/// early leaks every string the array held.
///
/// # Safety
/// `array` must be null or a live array of string handles built with `esize`,
/// and is freed here.
#[must_use]
pub unsafe fn take_string_array(array: KArray, esize: i64) -> Vec<String> {
    let stride = esize.max(0) as usize;
    // SAFETY: the caller vouches for the array.
    let len = unsafe { kira_rt_array_len(array) }.max(0) as usize;
    let mut texts = Vec::with_capacity(len);
    if !array.is_null() {
        // SAFETY: a live array's item block holds `len` slots of `stride` bytes.
        let items = unsafe { (*array).items };
        for index in 0..len {
            if items.is_null() {
                break;
            }
            // SAFETY: `index < len`, and a string element is one handle word.
            let handle = unsafe { items.add(index * stride).cast::<KStr>().read() };
            // SAFETY: the handle is live until the array below releases it.
            texts.push(unsafe { text_of(handle) });
        }
    }
    // SAFETY: the elements are string handles, so each is released by the leaf.
    unsafe { kira_rt_array_free(array, stride, Some(free_string_element)) };
    texts
}

/// Frees one string-handle element of an array.
///
/// # Safety
/// `slot` must address one live string-handle element.
unsafe extern "C" fn free_string_element(slot: *mut u8) {
    // SAFETY: the caller vouches for the slot.
    let handle = unsafe { slot.cast::<KStr>().read() };
    // SAFETY: the handle came from this runtime and is freed once.
    unsafe { release(handle) };
}

/// Writes each of `count` slots of a freshly built array.
///
/// # Safety
/// `array` must be a live array with at least `count` slots of `stride` bytes,
/// and `write` must initialize exactly one element at the address it is handed.
pub unsafe fn write_slots(
    array: KArray,
    count: usize,
    stride: usize,
    mut write: impl FnMut(*mut u8, usize),
) {
    if array.is_null() || count == 0 {
        return;
    }
    // SAFETY: the caller guarantees a live array built with this stride.
    let items = unsafe { (*array).items };
    if items.is_null() {
        return;
    }
    for index in 0..count {
        // SAFETY: `index < count <= len`, so the offset lands inside the block.
        write(unsafe { items.add(index * stride) }, index);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The stride generated code gives a handle element on this target.
    const HANDLE: i64 = size_of::<KStr>() as i64;

    #[test]
    fn a_string_array_round_trips_through_the_runtime() {
        let texts = vec!["first".to_owned(), String::new(), "third".to_owned()];
        // SAFETY: `HANDLE` is this target's handle stride, and the array below
        // is freed exactly once by the reader.
        let array = unsafe { string_array(&texts, HANDLE) };
        // SAFETY: the array was just built with this stride.
        let read = unsafe { take_string_array(array, HANDLE) };
        assert_eq!(read, texts);
    }

    #[test]
    fn a_null_array_reads_as_empty_rather_than_trapping() {
        // SAFETY: a null handle is an accepted input.
        let read = unsafe { take_string_array(std::ptr::null_mut(), HANDLE) };
        assert!(read.is_empty());
    }

    #[test]
    fn text_crosses_a_handle_intact() {
        let handle = handle_of("hello");
        // SAFETY: the handle is live and is freed once below.
        let text = unsafe { text_of(handle) };
        // SAFETY: same.
        unsafe { release(handle) };
        assert_eq!(text, "hello");
    }
}
