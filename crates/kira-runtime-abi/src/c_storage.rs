//! C storage that outlives the call it is handed to.
//!
//! A `CString` **parameter** is transient: C reads it during the call and Kira
//! frees it after. A `CString` **member of a C-layout struct** cannot be, and
//! the difference is not a preference. A struct like sokol's `sapp_desc` is
//! handed over once and read for the rest of the program's life, so a pointer
//! valid only for the call that passed it would be read after free — a
//! use-after-free with no Kira-visible cause, which is the worst failure this
//! seam can produce.
//!
//! So the storage here is **never released**. That is a deliberate leak, and it
//! is the only answer that makes the pointer safe: nothing else in the process
//! knows when C stops reading. Leaking is safe in Rust — it produces no
//! dangling pointer and no undefined behaviour — where freeing on any schedule
//! this side can guess is not.
//!
//! The cost is bounded by how many distinct strings a program hands to C, which
//! for the descriptor structs this exists for is a handful at startup. A program
//! that builds one per frame would grow without bound; that is worth knowing and
//! is why the refusal it replaces was there.

use std::ffi::CString;

/// Copies `text` into NUL-terminated storage that lives as long as the process,
/// returning its address as a pointer word.
///
/// Zero — a C `NULL` — when `text` contains an interior NUL, because the bytes
/// C would read then are not the bytes Kira holds, and handing over a truncated
/// string silently is worse than handing over nothing. This matches what the
/// transient parameter path does with the same input.
pub fn retain_text(text: &str) -> u64 {
    let Ok(owned) = CString::new(text) else {
        return 0;
    };
    // The allocation is deliberately never reclaimed; see the module docs.
    CString::into_raw(owned) as usize as u64
}

/// Copies `bytes` into storage that lives as long as the process, returning its
/// address as a pointer word.
///
/// The C-layout image of a struct handed to C **by pointer**. The same rule as
/// [`retain_text`] and for the same reason: nothing on this side knows whether
/// the callee kept the pointer, and a buffer freed when the call returns is a
/// dangling pointer for every callee that did. Zero for an empty image, which
/// no C-layout struct has.
pub fn retain_bytes(bytes: &[u8]) -> u64 {
    if bytes.is_empty() {
        return 0;
    }
    // Deliberately never reclaimed; see the module docs.
    let leaked: &'static mut [u8] = Box::leak(bytes.to_vec().into_boxed_slice());
    leaked.as_mut_ptr() as usize as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retained_storage_holds_the_text_and_a_terminator() {
        let word = retain_text("abc");
        assert_ne!(word, 0);
        // SAFETY: `retain` just produced this pointer from a live `CString` it
        // leaked, so it addresses a NUL-terminated buffer for the rest of the
        // process.
        let read = unsafe { std::ffi::CStr::from_ptr(word as usize as *const std::ffi::c_char) };
        assert_eq!(read.to_bytes(), b"abc");
    }

    #[test]
    fn two_retains_are_two_independent_buffers() {
        let left = retain_text("alpha");
        let right = retain_text("bravo");
        assert_ne!(left, right);
        // SAFETY: both words came from `retain` and address live storage.
        unsafe {
            let left = std::ffi::CStr::from_ptr(left as usize as *const std::ffi::c_char);
            let right = std::ffi::CStr::from_ptr(right as usize as *const std::ffi::c_char);
            assert_eq!(left.to_bytes(), b"alpha");
            assert_eq!(right.to_bytes(), b"bravo");
        }
    }

    #[test]
    fn an_interior_nul_is_refused_as_null_rather_than_truncated() {
        assert_eq!(retain_text("a\0b"), 0);
    }

    #[test]
    fn retained_bytes_hold_the_image_they_were_given() {
        let word = retain_bytes(&[1, 2, 3, 4]);
        assert_ne!(word, 0);
        // SAFETY: `retain_bytes` just leaked this buffer, so it addresses four
        // initialized bytes for the rest of the process.
        let read = unsafe { std::slice::from_raw_parts(word as usize as *const u8, 4) };
        assert_eq!(read, &[1, 2, 3, 4]);
        assert_eq!(retain_bytes(&[]), 0);
    }

    #[test]
    fn the_empty_string_is_a_real_pointer_not_null() {
        // `""` and "no string" are different values, and C tells them apart.
        let word = retain_text("");
        assert_ne!(word, 0);
        // SAFETY: as above.
        let read = unsafe { std::ffi::CStr::from_ptr(word as usize as *const std::ffi::c_char) };
        assert!(read.to_bytes().is_empty());
    }
}
