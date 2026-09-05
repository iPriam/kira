//! The rules for storage Kira hands to C.
//!
//! Every pointer Kira materializes for a foreign callee — a NUL-terminated
//! string, a C-layout struct image, an array flattened to C widths — is backed
//! by a **uniquely owned block**. The block belongs to exactly one Kira value:
//! the struct whose member it fills, or the call-argument temporary built at
//! the seam. It lives exactly as long as that value, is deep-cloned on the rare
//! true copy, and is freed when the value drops. There is no reference count
//! and no process-lifetime leak; the ownership checker's move rules are what
//! make the single owner real.
//!
//! Two lifetimes exist at a call, decided by the extern declaration:
//!
//! * **Borrowed** (the default): C reads the pointer during the call and never
//!   keeps it. The storage stays with its Kira owner and is freed when that
//!   owner dies.
//! * **Retained** (`retains: <param>` in the `@FFI.Extern` block): the callee
//!   keeps the pointer. The argument is a consuming parameter — the call site
//!   writes `move` — and ownership of every reachable block transfers to the
//!   engine's retained registry. The VM releases it with the instance, hybrid
//!   with the native session, and a whole-process native program at process
//!   teardown. No release overlaps a foreign call in flight.
//!
//! Each engine owns its blocks in its native idiom: the VM as a heap object
//! kind accounted by `HeapStats`, generated native code through the
//! `kira_rt_cblock_*` family in `kira-native-bridge`. What this module owns is
//! the vocabulary both share: the NUL rule for text crossing the seam, and the
//! one safe read out of storage C owns.

/// Copies `text` into a NUL-terminated byte image for the C side of the seam.
///
/// `None` when `text` contains an interior NUL: the bytes C would read then
/// are not the bytes Kira holds, and handing over a truncated string silently
/// is worse than handing over nothing. Every path that builds a C string —
/// transient argument, struct member, either engine — goes through this one
/// rule so the refusal is identical everywhere.
#[must_use]
pub fn nul_terminated(text: &str) -> Option<Vec<u8>> {
    if text.as_bytes().contains(&0) {
        return None;
    }
    let mut bytes = Vec::with_capacity(text.len() + 1);
    bytes.extend_from_slice(text.as_bytes());
    bytes.push(0);
    Some(bytes)
}

/// Reads `size` bytes at `address + offset` out of storage C owns.
///
/// `None` for a null base, which is the one bad pointer a Kira program can
/// actually produce: `nullPointer()` is spellable, and a C callback may hand
/// over a null for an optional argument. Every other pointer here came from the
/// foreign seam, and Kira has no pointer arithmetic to corrupt one with.
///
/// # Safety
///
/// `address` must be either null or a valid pointer to at least
/// `offset + size` readable bytes.
#[must_use]
pub unsafe fn read_bytes(address: u64, offset: u32, size: u32) -> Option<[u8; 8]> {
    if address == 0 {
        return None;
    }
    let size = size as usize;
    debug_assert!(size <= 8, "a seam scalar is at most eight bytes");
    let mut word = [0u8; 8];
    // SAFETY: the caller guarantees `address` addresses at least
    // `offset + size` readable bytes, and the null case returned above. The copy
    // is unaligned because a C struct's member is aligned within *its* layout
    // and the base came from C, neither of which is provable here.
    unsafe {
        let source = (address as *const u8).add(offset as usize);
        std::ptr::copy_nonoverlapping(source, word.as_mut_ptr(), size);
    }
    Some(word)
}


/// The byte a Kira `Bool` crosses the C seam as: `1` for `true`, `0` for
/// `false`, and no other value.
///
/// C `_Bool` is one byte holding exactly `0` or `1`; every other bit pattern is
/// outside the type's value set. Kira never writes one, so a C callee reading
/// the object as `_Bool`, comparing it to `1`, or indexing with it sees what it
/// would have seen from C.
#[must_use]
pub const fn c_bool_byte(value: bool) -> u8 {
    value as u8
}

/// The Kira `Bool` a `_Bool` byte C wrote reads back as: `true` for any nonzero
/// byte.
///
/// C leaves a `_Bool` outside `{0, 1}` undefined, so a library that writes one
/// has already left the standard. Reading the low bit and reading the whole byte
/// then answer differently, and two Kira engines reading it two ways is silent
/// corruption rather than a refusal. Nonzero is true, which is what `if (b)`
/// does, and it is the one rule every engine implements: the VM here, the
/// bundled libffi seam, and the code the native backend emits.
#[must_use]
pub const fn bool_from_c_byte(byte: u8) -> bool {
    byte != 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nul_terminated_appends_exactly_one_terminator() {
        assert_eq!(nul_terminated("abc").as_deref(), Some(&b"abc\0"[..]));
        assert_eq!(nul_terminated("").as_deref(), Some(&b"\0"[..]));
    }

    #[test]
    fn an_interior_nul_is_refused_rather_than_truncated() {
        assert_eq!(nul_terminated("a\0b"), None);
    }

    #[test]
    fn read_bytes_refuses_a_null_base() {
        // SAFETY: a null base is the refused case and is never dereferenced.
        assert_eq!(unsafe { read_bytes(0, 4, 4) }, None);
    }

    #[test]
    fn read_bytes_copies_the_addressed_scalar() {
        let storage: [u8; 12] = [0, 0, 0, 0, 1, 2, 3, 4, 0, 0, 0, 0];
        // SAFETY: `storage` is a live local with 12 readable bytes and the
        // read covers bytes 4..8.
        let word = unsafe { read_bytes(storage.as_ptr() as usize as u64, 4, 4) };
        assert_eq!(word, Some([1, 2, 3, 4, 0, 0, 0, 0]));
    }

    #[test]
    fn a_bool_crosses_as_exactly_one_or_zero() {
        assert_eq!(c_bool_byte(true), 1);
        assert_eq!(c_bool_byte(false), 0);
    }

    #[test]
    fn every_nonzero_byte_reads_back_as_true() {
        assert!(!bool_from_c_byte(0));
        for byte in 1..=u8::MAX {
            assert!(bool_from_c_byte(byte), "byte {byte} must read as true");
        }
    }
}
