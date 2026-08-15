//! The native capture-cell runtime: shared, mutable storage for a captured
//! `var`.
//!
//! # What a cell is
//!
//! A closure that captures a mutable binding has to see the enclosing frame's
//! writes, and the frame has to see the closure's. Nothing else in this runtime
//! shares mutable storage — a struct copies deeply, a string is never written
//! after it is built, and an array's block is shared only until a writer buys
//! its own — so a cell is a new shape rather than an existing one reused.
//!
//! # The box is an enum box
//!
//! [`KiraEnum`] is `{ tag, payload_kind, payload, shares }`. A cell needs
//! everything but the tag, so a cell **is** one of those boxes with the tag left
//! at zero. That is not a shortcut: it means the share bump generated code
//! emits inline, the payload-kind switch that decides what a release reclaims,
//! and the free path itself are all the ones enums already proved, with no
//! second ownership implementation to keep in step.
//!
//! ```text
//!   payload_kind  what `payload` is, and so what a read and a release owe it
//!   payload       the held value, type-erased into one word
//!   shares        how many values hold this box
//! ```
//!
//! # The contract these symbols keep
//!
//! - **A read is owned.** [`kira_rt_cell_get`] hands back a value the caller
//!   releases, cloning a `String`, enum, or erased payload on the way out. A
//!   borrowing read would let a write through another holder free the payload
//!   while the caller still had it — and a cell exists precisely so that other
//!   holders exist.
//! - **A write is one call.** [`kira_rt_cell_set`] releases the old payload and
//!   stores the new one, in that order, with nothing between them. Two calls
//!   would leave the box holding a freed handle for the window in between, and
//!   a trap in that window leaves it there for good. Nothing is ever handed a
//!   pointer *into* the payload slot, for the same reason.
//! - **A wide value goes out of line.** A struct or an array payload is
//!   [`PAYLOAD_AGGREGATE`]: erased bytes plus the clone and free leaves the
//!   backend generated for that concrete type, exactly as an enum's struct
//!   payload works.
//!
//! # Cycles leak
//!
//! A cell holding a closure that captures the same cell is a genuine reference
//! cycle, and share counts cannot collect one. The box and everything it
//! reaches leak: memory-safe, never freed twice, never freed early, and never
//! reclaimed. Collecting it needs a tracing collector this runtime does not
//! have, so it is recorded rather than defended against.
//!
//! Every symbol here is `extern "C"` with a `kira_rt_` prefix and a fixed
//! signature. These names are a wire contract with the backend's lowering and
//! are append-only: never rename one or change a signature in place.

use crate::array::{ElemClone, ElemFree};
use crate::enums::{
    AggregatePayload, KEnum, PAYLOAD_AGGREGATE, free_aggregate, kira_rt_enum_free,
    kira_rt_enum_new, kira_rt_enum_payload, kira_rt_enum_payload_aggregate, move_aggregate,
};

/// A Kira capture cell at the native ABI: an opaque owned handle.
///
/// The same handle a [`KEnum`] is, because the box is the same box. Spelled as
/// its own name so a signature says which of the two it means.
pub type KCell = KEnum;

/// The tag every cell box carries.
///
/// A cell has no variants, so the enum box's discriminant is unused. Fixing it
/// at zero rather than leaving it arbitrary keeps a cell box byte-identical
/// whatever built it, which is what makes the layout test below meaningful.
const CELL_TAG: i64 = 0;
const VM_CELL_PROXY_TAG: i64 = i64::MIN;

/// Boxes `payload` into a fresh cell holding one hold.
///
/// `payload_kind` says what the box takes ownership of, using the same
/// [`crate::enums`] constants an enum payload uses: inert bits for a scalar, an
/// owned string handle, an owned enum or erased handle. A wide value uses
/// [`kira_rt_cell_new_aggregate`] instead.
#[unsafe(no_mangle)]
pub extern "C" fn kira_rt_cell_new(payload_kind: i64, payload: u64) -> KCell {
    kira_rt_enum_new(CELL_TAG, payload_kind, payload)
}

/// Creates a native handle carrying a VM cell word for a callback round trip.
#[unsafe(no_mangle)]
pub extern "C" fn kira_rt_cell_vm_proxy_new(handle: u64) -> KCell {
    kira_rt_enum_new(VM_CELL_PROXY_TAG, crate::enums::PAYLOAD_INERT, handle)
}

/// Reads a VM cell word from a proxy, or `u64::MAX` for another handle.
///
/// # Safety
/// `value` must be null or a live cell handle from this runtime.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_rt_cell_vm_proxy_handle(value: KCell) -> u64 {
    if value.is_null() || crate::enums::is_inline(value) {
        return u64::MAX;
    }
    // SAFETY: the caller guarantees a live native cell box.
    if unsafe { crate::enums::kira_rt_enum_tag(value) } != VM_CELL_PROXY_TAG {
        return u64::MAX;
    }
    // SAFETY: the proxy stores its VM word as an inert payload.
    unsafe { kira_rt_enum_payload(value) }
}

/// Boxes a wide value into a fresh cell by moving its bytes into erased runtime
/// storage.
///
/// The backend passes clone/free leaves generated for the concrete type; null
/// means a flat bit-copy is sufficient and nothing owns storage.
///
/// # Safety
/// `source` must point to `size` readable bytes of one live Kira value. That
/// value's ownership transfers to the cell and must not be released through
/// `source` afterwards. The callbacks must match its type.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_rt_cell_new_aggregate(
    source: *const u8,
    size: usize,
    clone: Option<ElemClone>,
    free: Option<ElemFree>,
) -> KCell {
    // SAFETY: the caller supplies the value's bytes and matching leaves.
    let payload = unsafe { move_aggregate(source, size, clone, free, None) };
    kira_rt_cell_new(PAYLOAD_AGGREGATE, payload as u64)
}

/// Reads what a cell holds as an **owned** word, leaving the cell untouched.
///
/// A `String`, enum, or erased payload comes back cloned, so the reader's copy
/// and the cell's survive each other. A scalar comes back by bits and owns
/// nothing. A null handle reads as zero.
///
/// # Safety
/// `value` must be null or a live handle from this runtime; it is left
/// untouched.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_rt_cell_get(value: KCell) -> u64 {
    // SAFETY: the caller's contract is the one `kira_rt_enum_payload` states,
    // and a cell box *is* an enum box.
    unsafe { kira_rt_enum_payload(value) }
}

/// Reads a wide payload into caller-owned storage as an independent value.
///
/// The cell keeps owning its payload; the clone leaf duplicates every nested
/// handle into `out`, so the reader may outlive a later write through the cell.
///
/// # Safety
/// `value` must be a live cell whose payload kind is [`PAYLOAD_AGGREGATE`], and
/// `out` must point at writable storage for that concrete type.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_rt_cell_get_aggregate(value: KCell, out: *mut u8) {
    // SAFETY: the caller's contract is the one `kira_rt_enum_payload_aggregate`
    // states.
    unsafe { kira_rt_enum_payload_aggregate(value, out) };
}

/// Replaces what a cell holds, releasing the old payload, in one call.
///
/// The old payload is read out of the box and the new one written in **before**
/// the old one is released, so the box never holds storage that is being freed.
/// A null handle is a no-op that releases the incoming payload, which keeps the
/// caller's hand-off total: whatever it passed is this call's on every path.
///
/// # Safety
/// `value` must be null or a live handle. `payload` must be an owned value of
/// the kind `payload_kind` names; its ownership transfers to the cell.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_rt_cell_set(value: KCell, payload_kind: i64, payload: u64) {
    if value.is_null() {
        // Nothing to store into, and the incoming value is already this call's
        // — so release it rather than leaking it.
        // SAFETY: the caller transferred an owned payload of this kind.
        unsafe { release_payload(payload_kind, payload) };
        return;
    }
    // SAFETY: a non-null handle is a live box the caller has not freed. The
    // read, the two writes, and the release below run with nothing in between
    // that could observe the box or trap.
    let (old_kind, old_payload) = unsafe {
        let cell = &mut *value;
        let previous = (cell.payload_kind_raw(), cell.payload_raw());
        cell.set_payload_raw(payload_kind, payload);
        previous
    };
    // SAFETY: the box no longer names it, and the box owned exactly one hold.
    unsafe { release_payload(old_kind, old_payload) };
}

/// Replaces what a cell holds with a wide value, releasing the old payload.
///
/// The store-back half of writing through a cell into a struct or an array: the
/// caller read the value out, wrote into its copy — which is where an array
/// buys elements of its own — and hands the possibly-new handle back here.
///
/// # Safety
/// The same contract as [`kira_rt_cell_new_aggregate`] for `source`, `size`,
/// and the leaves, plus [`kira_rt_cell_set`]'s for `value`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_rt_cell_set_aggregate(
    value: KCell,
    source: *const u8,
    size: usize,
    clone: Option<ElemClone>,
    free: Option<ElemFree>,
) {
    // SAFETY: the caller supplies the value's bytes and matching leaves.
    let payload = unsafe { move_aggregate(source, size, clone, free, None) };
    // SAFETY: the payload is a live erased box this call just took ownership of.
    unsafe { kira_rt_cell_set(value, PAYLOAD_AGGREGATE, payload as u64) };
}

/// Releases one hold on a cell, freeing the box and its payload once no value
/// holds it. A null handle is a no-op.
///
/// # Safety
/// `value` must be null or a live handle from this runtime, released once per
/// copy of it that was made.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_rt_cell_free(value: KCell) {
    // SAFETY: the caller's contract is the one `kira_rt_enum_free` states, and
    // a cell box *is* an enum box — including the payload-kind switch that
    // decides what the last release reclaims.
    unsafe { kira_rt_enum_free(value) };
}

/// Releases one owned payload word of the kind that names it.
///
/// The half of the enum box's free path that is about the payload rather than
/// the box, reached here without going through a box at all.
///
/// # Safety
/// `payload` must be an owned value of the kind `payload_kind` names, released
/// exactly once.
unsafe fn release_payload(payload_kind: i64, payload: u64) {
    match payload_kind {
        // SAFETY: the kind promises a live `KStr`, released here exactly once.
        crate::enums::PAYLOAD_STR => unsafe {
            crate::runtime::kira_rt_str_free(payload as crate::runtime::KStr)
        },
        // SAFETY: the kind promises a live `KEnum`, released here exactly once.
        crate::enums::PAYLOAD_ENUM => unsafe { kira_rt_enum_free(payload as KEnum) },
        // SAFETY: the kind promises a live erased payload, owned exactly once.
        PAYLOAD_AGGREGATE => unsafe { free_aggregate(payload as *mut AggregatePayload) },
        // An unrecognized kind is treated as inert, which leaks rather than
        // corrupting — the conservative direction for a word this code cannot
        // interpret, and the same choice `kira_rt_enum_new` documents.
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::enums::{KiraEnum, PAYLOAD_INERT, PAYLOAD_STR};
    use crate::runtime::{KStr, kira_rt_str_free, kira_rt_str_len, kira_rt_str_new};

    /// Builds a string handle, as the backend's lowering would.
    fn str_handle(text: &str) -> KStr {
        // SAFETY: the slice covers exactly `len` readable bytes.
        unsafe { kira_rt_str_new(text.as_ptr(), text.len()) }
    }

    /// The layout the backend mirrors, pinned here because the two are compiled
    /// separately and nothing but a test makes them agree.
    /// The layout the backend mirrors as `Types::enum_box` and reaches into for
    /// the inline share bump, pinned here because the two are compiled
    /// separately and nothing but a test makes them agree.
    #[test]
    fn a_cell_box_is_an_enum_box() {
        // `{ i64 tag, i64 payload_kind, i64 payload, usize shares }`.
        assert_eq!(
            size_of::<KiraEnum>(),
            3 * size_of::<i64>() + size_of::<usize>()
        );
        assert_eq!(align_of::<KiraEnum>(), 8);
        // A handle is one pointer, whichever of the two names it goes by.
        assert_eq!(size_of::<KCell>(), size_of::<KEnum>());
    }

    #[test]
    fn a_write_through_one_hold_is_visible_through_the_other() {
        // The whole point of the type: two values, one storage.
        // SAFETY: every handle below is live and released once per hold.
        unsafe {
            let cell = kira_rt_cell_new(PAYLOAD_INERT, 1);
            let shared = crate::enums::kira_rt_enum_clone(cell);
            assert_eq!(cell, shared, "a copy is the same box, held twice");

            kira_rt_cell_set(cell, PAYLOAD_INERT, 42);
            assert_eq!(kira_rt_cell_get(shared), 42);

            kira_rt_cell_free(cell);
            // The box outlives the first release, because the second hold is
            // still reading it.
            assert_eq!(kira_rt_cell_get(shared), 42);
            kira_rt_cell_free(shared);
        }
    }

    #[test]
    fn a_replaced_string_payload_is_released_exactly_once() {
        // Under Miri or ASan, forgetting the release surfaces as a leak and
        // releasing it twice as a double free; both are what `cell_set` owning
        // the old payload is for.
        // SAFETY: every handle below is live and released exactly once.
        unsafe {
            let cell = kira_rt_cell_new(PAYLOAD_STR, str_handle("first") as u64);
            kira_rt_cell_set(cell, PAYLOAD_STR, str_handle("second") as u64);

            let read = kira_rt_cell_get(cell) as KStr;
            assert_eq!(kira_rt_str_len(read), 6, "the second value is what is held");
            kira_rt_str_free(read);
            kira_rt_cell_free(cell);
        }
    }

    #[test]
    fn a_read_outlives_the_cell_it_came_from() {
        // The invariant that makes the read *owned*: a caller holding a payload
        // must survive the cell being written through or released.
        // SAFETY: every handle below is live and released exactly once.
        unsafe {
            let cell = kira_rt_cell_new(PAYLOAD_STR, str_handle("held") as u64);
            let read = kira_rt_cell_get(cell) as KStr;
            kira_rt_cell_set(cell, PAYLOAD_STR, str_handle("replaced") as u64);
            assert_eq!(
                kira_rt_str_len(read),
                4,
                "the read survived the write that replaced it"
            );
            kira_rt_str_free(read);
            kira_rt_cell_free(cell);
        }
    }

    #[test]
    fn a_null_cell_releases_the_value_handed_to_it() {
        // Total hand-off: whatever the caller passed is this call's on every
        // path, so a refusal cannot leak it.
        // SAFETY: the string is live and released exactly once, by the call.
        unsafe {
            kira_rt_cell_set(
                std::ptr::null_mut(),
                PAYLOAD_STR,
                str_handle("dropped") as u64,
            );
        }
    }
}
