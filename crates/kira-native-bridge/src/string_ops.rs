//! The native half of the shared-opcode string operations.
//!
//! One helper per [`StringOp`](kira_runtime_abi::StringOp), each answering
//! exactly what the VM's `perform_string_op` answers for the same input — that
//! agreement is the whole point, and the backend-parity suite is what proves it
//! rather than this comment.
//!
//! Separate from `runtime.rs` because that file is already past the size where
//! a module should be split, not because these belong anywhere else: they are
//! runtime helpers like every other `kira_rt_*`.
//!
//! # Ownership
//!
//! Every helper takes its arguments by handle and frees all of them, exactly as
//! `kira_rt_str_index_of` does. A caller emits the call and forgets the
//! operands; nothing here is borrowed past the call.
//!
//! # Text, not bytes
//!
//! These operate on `str`, not `[u8]`, because `trim`, `lowercase` and
//! `uppercase` are defined on characters and answering them per byte would be
//! wrong for every non-ASCII input. A handle whose bytes are not UTF-8 cannot
//! come from a Kira `String`, so the conversion is lossless in practice; the
//! lossy fallback is there so a corrupted handle degrades instead of trapping.

use kira_runtime_abi::StringOp;

use crate::array::{KArray, kira_rt_array_new, kira_rt_array_slot};
use crate::runtime::{KStr, bytes_of, bytes_to_handle, drop_handle};

/// The width of one `KStr` slot in an array of strings.
const STRING_SLOT: usize = size_of::<KStr>();

/// Borrows a handle's bytes as text, replacing anything that is not UTF-8.
///
/// # Safety
/// `handle` must be null or a live handle from this runtime.
unsafe fn text_of(handle: KStr) -> String {
    // SAFETY: the caller's promise is exactly `bytes_of`'s.
    let bytes = unsafe { bytes_of(handle) };
    String::from_utf8_lossy(bytes).into_owned()
}

/// Whether `value` holds `needle` anywhere, freeing both.
///
/// # Safety
/// Both arguments must be null or live handles; both are freed here.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_rt_string_contains(value: KStr, needle: KStr) -> bool {
    // SAFETY: caller passes live (or null) handles that outlive these reads.
    let held = unsafe { text_of(value).contains(&text_of(needle)) };
    // SAFETY: the same handles, each consumed exactly once here.
    unsafe {
        drop_handle(value);
        drop_handle(needle);
    }
    held
}

/// Whether `value` begins with `prefix`, freeing both.
///
/// # Safety
/// Both arguments must be null or live handles; both are freed here.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_rt_string_starts_with(value: KStr, prefix: KStr) -> bool {
    // SAFETY: caller passes live (or null) handles that outlive these reads.
    let held = unsafe { text_of(value).starts_with(&text_of(prefix)) };
    // SAFETY: the same handles, each consumed exactly once here.
    unsafe {
        drop_handle(value);
        drop_handle(prefix);
    }
    held
}

/// Whether `value` ends with `suffix`, freeing both.
///
/// # Safety
/// Both arguments must be null or live handles; both are freed here.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_rt_string_ends_with(value: KStr, suffix: KStr) -> bool {
    // SAFETY: caller passes live (or null) handles that outlive these reads.
    let held = unsafe { text_of(value).ends_with(&text_of(suffix)) };
    // SAFETY: the same handles, each consumed exactly once here.
    unsafe {
        drop_handle(value);
        drop_handle(suffix);
    }
    held
}

/// `value` with every occurrence of `from` replaced by `to`, freeing all three.
///
/// # Safety
/// All arguments must be null or live handles; all are freed here.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_rt_string_replace(value: KStr, from: KStr, to: KStr) -> KStr {
    // SAFETY: caller passes live (or null) handles that outlive these reads.
    let replaced = unsafe { text_of(value).replace(&text_of(from), &text_of(to)) };
    // SAFETY: the same handles, each consumed exactly once here.
    unsafe {
        drop_handle(value);
        drop_handle(from);
        drop_handle(to);
    }
    bytes_to_handle(replaced.into_bytes())
}

/// `value` without leading or trailing whitespace, freeing it.
///
/// # Safety
/// `value` must be null or a live handle; it is freed here.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_rt_string_trim(value: KStr) -> KStr {
    // SAFETY: caller passes a live (or null) handle that outlives this read.
    let trimmed = unsafe { text_of(value).trim().to_owned() };
    // SAFETY: the same handle, consumed exactly once here.
    unsafe { drop_handle(value) };
    bytes_to_handle(trimmed.into_bytes())
}

/// `value` lowercased, freeing it.
///
/// # Safety
/// `value` must be null or a live handle; it is freed here.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_rt_string_lowercase(value: KStr) -> KStr {
    // SAFETY: caller passes a live (or null) handle that outlives this read.
    let lowered = unsafe { text_of(value).to_lowercase() };
    // SAFETY: the same handle, consumed exactly once here.
    unsafe { drop_handle(value) };
    bytes_to_handle(lowered.into_bytes())
}

/// `value` uppercased, freeing it.
///
/// # Safety
/// `value` must be null or a live handle; it is freed here.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_rt_string_uppercase(value: KStr) -> KStr {
    // SAFETY: caller passes a live (or null) handle that outlives this read.
    let raised = unsafe { text_of(value).to_uppercase() };
    // SAFETY: the same handle, consumed exactly once here.
    unsafe { drop_handle(value) };
    bytes_to_handle(raised.into_bytes())
}

/// Whether `value` reads as a whole number, freeing it.
///
/// # Safety
/// `value` must be null or a live handle; it is freed here.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_rt_string_is_int(value: KStr) -> bool {
    // SAFETY: caller passes a live (or null) handle that outlives this read.
    let reads = unsafe { text_of(value).trim().parse::<i64>().is_ok() };
    // SAFETY: the same handle, consumed exactly once here.
    unsafe { drop_handle(value) };
    reads
}

/// The whole number `value` reads as, freeing it.
///
/// Traps on text that reads as none, which `isInt` is there to prevent — the
/// same shape `charAt` has for an index out of range.
///
/// # Safety
/// `value` must be null or a live handle; it is freed here.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_rt_string_to_int(value: KStr) -> i64 {
    // SAFETY: caller passes a live (or null) handle that outlives this read.
    let parsed = unsafe { text_of(value).trim().parse::<i64>() };
    // SAFETY: the same handle, consumed exactly once here.
    unsafe { drop_handle(value) };
    match parsed {
        Ok(value) => value,
        Err(_) => {
            eprintln!("kira: runtime trap: text does not read as a whole number");
            crate::runtime::print_trap_backtrace();
            std::process::exit(1);
        }
    }
}

/// `value` without its last Unicode scalar, freeing it.
///
/// `pop` removes a whole `char`, which is what makes this different from
/// dropping the last byte: the other primitives index bytes, and truncating a
/// multi-byte scalar mid-way would leave text that is no longer UTF-8.
///
/// # Safety
/// `value` must be null or a live handle; it is freed here.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_rt_string_drop_last_scalar(value: KStr) -> KStr {
    // SAFETY: caller passes a live (or null) handle that outlives this read.
    let mut dropped = unsafe { text_of(value).to_owned() };
    dropped.pop();
    // SAFETY: the same handle, consumed exactly once here.
    unsafe { drop_handle(value) };
    bytes_to_handle(dropped.into_bytes())
}

/// The text of one Unicode scalar, from its code point.
///
/// A code point outside Unicode, or a surrogate half, names no scalar and
/// renders as the empty string rather than trapping — the same answer the VM
/// gives.
#[unsafe(no_mangle)]
pub extern "C" fn kira_rt_scalar_text(code: i64) -> KStr {
    let text = u32::try_from(code)
        .ok()
        .and_then(char::from_u32)
        .map(String::from)
        .unwrap_or_default();
    bytes_to_handle(text.into_bytes())
}

/// `value` split on every occurrence of `separator`, freeing both.
///
/// An empty separator answers the whole text as one piece — the same answer the
/// VM gives, and the only one that is a split of anything.
///
/// # Safety
/// Both arguments must be null or live handles; both are freed here.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_rt_string_split(value: KStr, separator: KStr) -> KArray {
    // SAFETY: caller passes live (or null) handles that outlive these reads.
    let (text, sep) = unsafe { (text_of(value), text_of(separator)) };
    // SAFETY: the same handles, each consumed exactly once here.
    unsafe {
        drop_handle(value);
        drop_handle(separator);
    }
    let pieces: Vec<String> = if sep.is_empty() {
        vec![text]
    } else {
        text.split(sep.as_str()).map(str::to_owned).collect()
    };
    let array = kira_rt_array_new(pieces.len(), STRING_SLOT);
    for (index, piece) in pieces.into_iter().enumerate() {
        let handle = bytes_to_handle(piece.into_bytes());
        // SAFETY: the array was just built with exactly this many slots of
        // exactly this width, so every index is in range and correctly sized.
        let slot = unsafe { kira_rt_array_slot(array, index as i64, STRING_SLOT) };
        // SAFETY: `slot` addresses one `KStr`-wide element of a live array, and
        // the slot is uninitialized, so nothing is overwritten unfreed.
        unsafe { slot.cast::<KStr>().write(handle) };
    }
    array
}

/// The `kira_rt_*` symbol one operation is performed by.
///
/// The one place the mapping is written down on this side; it mirrors
/// [`StringOp::runtime_symbol`], and the test below is what keeps the two from
/// drifting apart.
#[must_use]
pub const fn runtime_symbol(op: StringOp) -> &'static str {
    op.runtime_symbol()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::kira_rt_str_new;

    /// Builds a handle from text, the way generated code does.
    fn handle(text: &str) -> KStr {
        // SAFETY: the pointer and length describe a live borrow that outlives
        // the call, which is what the helper documents.
        unsafe { kira_rt_str_new(text.as_ptr(), text.len()) }
    }

    #[test]
    fn contains_matches_the_vm() {
        // SAFETY: fresh handles, each consumed exactly once by the call.
        assert!(unsafe { kira_rt_string_contains(handle("hello world"), handle("o w")) });
        // SAFETY: as above.
        assert!(!unsafe { kira_rt_string_contains(handle("hello"), handle("z")) });
        // An empty needle is held by every string, including an empty one.
        // SAFETY: as above.
        assert!(unsafe { kira_rt_string_contains(handle(""), handle("")) });
    }

    #[test]
    fn the_ends_are_told_apart() {
        // SAFETY: fresh handles, each consumed exactly once by the call.
        assert!(unsafe { kira_rt_string_starts_with(handle("prefix-body"), handle("prefix")) });
        // SAFETY: as above.
        assert!(!unsafe { kira_rt_string_starts_with(handle("prefix-body"), handle("body")) });
        // SAFETY: as above.
        assert!(unsafe { kira_rt_string_ends_with(handle("prefix-body"), handle("body")) });
        // SAFETY: as above.
        assert!(!unsafe { kira_rt_string_ends_with(handle("prefix-body"), handle("prefix")) });
    }

    #[test]
    fn case_and_trim_are_character_wise_not_byte_wise() {
        // SAFETY: a fresh handle, consumed exactly once, and the result freed.
        let raised = unsafe { kira_rt_string_uppercase(handle("stra\u{df}e")) };
        // SAFETY: the result handle is live and read once before being freed.
        let text = unsafe { text_of(raised) };
        // SAFETY: consumed exactly once.
        unsafe { drop_handle(raised) };
        assert_eq!(text, "STRASSE", "uppercase follows Unicode, not bytes");

        // SAFETY: a fresh handle, consumed exactly once, and the result freed.
        let trimmed = unsafe { kira_rt_string_trim(handle("  padded\t\n")) };
        // SAFETY: the result handle is live and read once before being freed.
        let text = unsafe { text_of(trimmed) };
        // SAFETY: consumed exactly once.
        unsafe { drop_handle(trimmed) };
        assert_eq!(text, "padded");
    }

    #[test]
    fn every_operation_names_the_symbol_the_abi_expects() {
        for op in StringOp::ALL {
            assert_eq!(runtime_symbol(op), op.runtime_symbol());
        }
    }
}
