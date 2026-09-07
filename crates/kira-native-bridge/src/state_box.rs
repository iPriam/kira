//! Native callback state held the way Rust holds it: one allocation, the value
//! inside it, fields addressed directly.
//!
//! # Why this exists beside the value-tree store
//!
//! [`crate::native_state`] keeps state as a backend-neutral tree, which is what
//! makes a value cross between the VM and native halves of a hybrid program.
//! It is the wrong shape for a frame budget. Recovering copied the whole tree,
//! reading one field walked it and copied a subtree, and writing one field
//! re-encoded the value from scratch — so a UI compositor recovering its batch
//! state per quad paid for its entire glyph cache to read one counter.
//!
//! A native engine needs none of that. It already has a layout for the value —
//! its own — so the state is a box holding exactly that, and a field is an
//! offset into it. Reading a field is a load. Writing one is a store. Nothing
//! is copied, encoded, or walked, which is the difference between a frame and
//! a stall.
//!
//! # The box
//!
//! ```text
//!   magic     recognizes a live box, and catches a freed one
//!   type_id   the program-stable type, checked on every recovery
//!   size      the payload's size, to deallocate with the layout it was made
//!   align     the payload's alignment, same reason
//!   free      drops what the payload's fields own, or null when none do
//!   payload   the value itself, in the backend's own layout
//! ```
//!
//! The token is the box's address with the low bit set. A box is
//! word-aligned so that bit is free, and the tree store hands out only even
//! tokens — one bit tells a caller which kind of state a token names, with no
//! lookup and no lock. See [`NativeStateToken::is_boxed`].

use std::alloc::{self, Layout};

use kira_runtime_abi::{NativeStateStatus, NativeStateToken};

/// Drops whatever the fields of one boxed state own.
///
/// The backend emits one of these per state type, for the same reason an array
/// gets a per-element leaf: the runtime owns the *box*, and only the compiler
/// knows which fields inside it own a handle.
pub type NativeStateBoxFree = unsafe extern "C" fn(*mut u8);

/// Recognizes a live box, and a use-after-free with it.
const MAGIC: u64 = 0x4b49_5241_5354_4231; // "KIRASTB1"

/// The header in front of a boxed state's payload.
#[repr(C)]
struct BoxHeader {
    magic: u64,
    type_id: u64,
    size: usize,
    align: usize,
    free: Option<NativeStateBoxFree>,
    /// Owners of the box: the Kira handle, every exported token, and every
    /// explicit retain. The release that takes it to zero frees the box.
    refs: u64,
}

/// The header's own layout, which every box starts with.
fn header_layout() -> Layout {
    Layout::new::<BoxHeader>()
}

/// The layout of a whole box holding a payload of `size` and `align`.
///
/// `None` when the payload's alignment is not a power of two or the total would
/// overflow — a malformed request from generated code, refused rather than
/// passed to the allocator.
fn box_layout(size: usize, align: usize) -> Option<(Layout, usize)> {
    let payload = Layout::from_size_align(size, align).ok()?;
    header_layout().extend(payload).ok()
}

/// Where the payload starts inside a box whose payload has `align`.
///
/// The same number [`box_layout`] returns, arrived at by rounding rather than by
/// building two `Layout`s to ask one of them. Recovery is not an occasional
/// call: a native UI frame recovers its compositor state once per emitted quad,
/// and `Layout::from_size_align` plus `extend` put alignment validation,
/// overflow checks and two `Result`s on that path for an offset that is fixed
/// the moment the box is allocated.
///
/// A live box always carries a power-of-two alignment —
/// [`kira_rt_native_state_box_new`] refuses anything else before the box
/// exists, so a header that fails the magic check never reaches here — which is
/// what makes the mask exact. `payload_offset_matches_the_layout` pins the two
/// against each other.
fn payload_offset(align: usize) -> usize {
    let align = align.max(1);
    (size_of::<BoxHeader>() + align - 1) & !(align - 1)
}

/// Allocates a box for a value of `size`/`align` and returns its token.
///
/// `free` drops what the payload's fields own when the state is released, and
/// is null when nothing in it owns anything.
///
/// Writes the token to `out` and returns [`NativeStateStatus::OK`], or leaves
/// `out` untouched and returns a status. The payload is *uninitialized*: the
/// caller stores the value into [`kira_rt_native_state_box_payload`] next, which
/// is what makes this one allocation rather than one plus a copy.
///
/// # Safety
/// `out` must be writable when non-null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_rt_native_state_box_new(
    type_id: u64,
    size: usize,
    align: usize,
    free: Option<NativeStateBoxFree>,
    out: *mut u64,
) -> u32 {
    if out.is_null() {
        return NativeStateStatus::MALFORMED_VALUE.0;
    }
    let Some((layout, offset)) = box_layout(size, align) else {
        return NativeStateStatus::MALFORMED_VALUE.0;
    };
    // SAFETY: the layout has a non-zero size — the header alone guarantees it.
    let base = unsafe { alloc::alloc(layout) };
    if base.is_null() {
        return NativeStateStatus::MALFORMED_VALUE.0;
    }
    // SAFETY: `base` is a fresh allocation of at least the header's size and
    // alignment, so writing the header through it is in bounds and aligned.
    unsafe {
        base.cast::<BoxHeader>().write(BoxHeader {
            magic: MAGIC,
            type_id,
            size,
            align,
            free,
            refs: 1,
        });
    }
    debug_assert!(offset >= size_of::<BoxHeader>());
    // SAFETY: the caller supplies one writable word.
    unsafe { *out = token_of(base) };
    NativeStateStatus::OK.0
}

/// Returns the address of the value inside the box `token` names.
///
/// Type-checked: a token of the wrong type, a token that is not a box, or one
/// already freed yields null and a status, never a pointer into the wrong
/// value. This is the whole recovery path — no copy, no decode — so a caller
/// reads and writes fields straight through the returned address.
///
/// # Safety
/// `out` must be writable when non-null, and `token` must be null, a live box
/// token from this runtime, or a token this runtime handed out.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_rt_native_state_box_payload(
    token: u64,
    type_id: u64,
    out: *mut *mut u8,
) -> u32 {
    if out.is_null() {
        return NativeStateStatus::MALFORMED_VALUE.0;
    }
    if token == 0 {
        return NativeStateStatus::NULL_TOKEN.0;
    }
    if !NativeStateToken::from_word(token).is_boxed() {
        return NativeStateStatus::UNKNOWN_TOKEN.0;
    }
    let base = base_of(token);
    // SAFETY: the token came from `kira_rt_native_state_box_new`, so it
    // addresses a live header until it is freed; the magic catches a stale one
    // before anything else is read.
    let header = unsafe { &*base.cast::<BoxHeader>() };
    if header.magic != MAGIC {
        return NativeStateStatus::UNKNOWN_TOKEN.0;
    }
    if header.type_id != type_id {
        return NativeStateStatus::WRONG_TYPE.0;
    }
    // SAFETY: the payload starts `payload_offset` bytes into an allocation that
    // covers the header and the payload both — the box was allocated with the
    // layout that offset comes from, and the magic above proved it is that box.
    let payload = unsafe { base.add(payload_offset(header.align)) };
    // SAFETY: the caller supplies one writable pointer slot.
    unsafe { *out = payload };
    NativeStateStatus::OK.0
}

/// Adds one owner to a boxed state.
///
/// # Safety
/// `token` must be null, a live box token from this runtime, or a token this
/// runtime handed out.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_rt_native_state_box_retain(token: u64) -> u32 {
    if token == 0 {
        return NativeStateStatus::NULL_TOKEN.0;
    }
    if !NativeStateToken::from_word(token).is_boxed() {
        return NativeStateStatus::UNKNOWN_TOKEN.0;
    }
    let base = base_of(token);
    // SAFETY: the token addresses a live header until its last release.
    let header = unsafe { &mut *base.cast::<BoxHeader>() };
    if header.magic != MAGIC {
        return NativeStateStatus::UNKNOWN_TOKEN.0;
    }
    match header.refs.checked_add(1) {
        Some(refs) => {
            header.refs = refs;
            NativeStateStatus::OK.0
        }
        None => NativeStateStatus::TOKEN_EXHAUSTED.0,
    }
}

/// Removes one owner from a boxed state; the last release drops what its
/// fields own, then frees the box.
///
/// The magic is cleared first, so releasing the same token past its last
/// owner is reported as an unknown token rather than corrupting the allocator.
///
/// # Safety
/// `token` must be null, a live box token from this runtime, or a token this
/// runtime handed out.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_rt_native_state_box_free(token: u64) -> u32 {
    if token == 0 {
        return NativeStateStatus::NULL_TOKEN.0;
    }
    if !NativeStateToken::from_word(token).is_boxed() {
        return NativeStateStatus::UNKNOWN_TOKEN.0;
    }
    let base = base_of(token);
    // SAFETY: the token addresses a live header until this frees it.
    let header = unsafe { &mut *base.cast::<BoxHeader>() };
    if header.magic != MAGIC {
        return NativeStateStatus::UNKNOWN_TOKEN.0;
    }
    if header.refs > 1 {
        header.refs -= 1;
        return NativeStateStatus::OK.0;
    }
    let Some((layout, offset)) = box_layout(header.size, header.align) else {
        return NativeStateStatus::MALFORMED_VALUE.0;
    };
    let free = header.free;
    header.magic = 0;
    if let Some(free) = free {
        // SAFETY: the payload is a live initialized value of the type this
        // leaf was emitted for, and it is dropped exactly once.
        unsafe { free(base.add(offset)) };
    }
    // SAFETY: `base` came from `alloc` with exactly this layout.
    unsafe { alloc::dealloc(base, layout) };
    NativeStateStatus::OK.0
}

/// Whether `token` names a box this runtime allocated.
///
/// Used by the shared release path, which serves both kinds of state.
#[must_use]
pub fn is_box_token(token: u64) -> bool {
    token != 0 && NativeStateToken::from_word(token).is_boxed()
}

/// The token for a box at `base`.
fn token_of(base: *mut u8) -> u64 {
    base as u64 | 1
}

/// The box address a token names.
fn base_of(token: u64) -> *mut u8 {
    (token & !1) as *mut u8
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The type id these tests box under; any stable word does.
    const TYPE: u64 = 77;

    /// The recovery path derives the payload offset by rounding instead of
    /// building the layout. It has to land on the same byte the box was
    /// allocated with, or every recovered field is read at the wrong address —
    /// so pin the two against each other across the alignments a backend
    /// emits, and sizes either side of the header.
    #[test]
    fn payload_offset_matches_the_layout() {
        for align in [1usize, 2, 4, 8, 16, 32, 64, 128] {
            for size in [0usize, 1, 7, 8, 40, 41, 1024] {
                let Some((_, offset)) = box_layout(size, align) else {
                    continue;
                };
                assert_eq!(payload_offset(align), offset, "size {size}, align {align}");
            }
        }
    }

    /// Allocates a box for one `i64` payload.
    fn new_box(free: Option<NativeStateBoxFree>) -> u64 {
        let mut token = 0;
        // SAFETY: `token` is writable.
        let status = unsafe {
            kira_rt_native_state_box_new(
                TYPE,
                size_of::<i64>(),
                align_of::<i64>(),
                free,
                &mut token,
            )
        };
        assert_eq!(status, NativeStateStatus::OK.0);
        assert!(is_box_token(token));
        token
    }

    /// The payload address of a live box.
    fn payload(token: u64, type_id: u64) -> Result<*mut u8, u32> {
        let mut out = std::ptr::null_mut();
        // SAFETY: `out` is writable and the token came from this runtime.
        let status = unsafe { kira_rt_native_state_box_payload(token, type_id, &mut out) };
        if status == NativeStateStatus::OK.0 {
            Ok(out)
        } else {
            Err(status)
        }
    }

    #[test]
    fn a_field_written_through_the_payload_reads_back() {
        let token = new_box(None);
        let at = payload(token, TYPE).expect("a live box");
        // SAFETY: the payload is one aligned, writable `i64`.
        unsafe { at.cast::<i64>().write(1234) };
        let again = payload(token, TYPE).expect("a live box");
        // SAFETY: the payload was just initialized.
        assert_eq!(unsafe { again.cast::<i64>().read() }, 1234);
        // SAFETY: the token is live and freed once.
        assert_eq!(unsafe { kira_rt_native_state_box_free(token) }, 0);
    }

    /// Recovering twice yields the same address, which is what makes a read a
    /// load rather than a copy — two recoveries are two views of one value.
    #[test]
    fn recovering_twice_addresses_the_same_value() {
        let token = new_box(None);
        let first = payload(token, TYPE).expect("a live box");
        let second = payload(token, TYPE).expect("a live box");
        assert_eq!(first, second);
        // SAFETY: the token is live and freed once.
        assert_eq!(unsafe { kira_rt_native_state_box_free(token) }, 0);
    }

    #[test]
    fn the_wrong_type_is_refused_rather_than_reinterpreted() {
        let token = new_box(None);
        assert_eq!(
            payload(token, TYPE + 1).expect_err("a type mismatch"),
            NativeStateStatus::WRONG_TYPE.0
        );
        // SAFETY: the token is live and freed once.
        assert_eq!(unsafe { kira_rt_native_state_box_free(token) }, 0);
    }

    #[test]
    fn a_freed_box_is_unknown_rather_than_read() {
        let token = new_box(None);
        // SAFETY: the token is live and freed once here.
        assert_eq!(unsafe { kira_rt_native_state_box_free(token) }, 0);
        assert_eq!(
            payload(token, TYPE).expect_err("a freed box"),
            NativeStateStatus::UNKNOWN_TOKEN.0
        );
        // SAFETY: the same token, whose header no longer carries the magic.
        let again = unsafe { kira_rt_native_state_box_free(token) };
        assert_eq!(again, NativeStateStatus::UNKNOWN_TOKEN.0);
    }

    /// A token the value-tree store handed out is even, so the box path must
    /// refuse it rather than treat the number as an address.
    #[test]
    fn a_store_token_is_not_mistaken_for_a_box() {
        assert!(!is_box_token(2));
        assert_eq!(
            payload(2, TYPE).expect_err("not a box"),
            NativeStateStatus::UNKNOWN_TOKEN.0
        );
        // SAFETY: two is not a box token, which this reports rather than reads.
        let refused = unsafe { kira_rt_native_state_box_free(2) };
        assert_eq!(refused, NativeStateStatus::UNKNOWN_TOKEN.0);
    }

    /// The field-drop leaf runs exactly once, when the box is released.
    #[test]
    fn the_free_leaf_runs_once_on_release() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        static RUNS: AtomicUsize = AtomicUsize::new(0);

        unsafe extern "C" fn count(_: *mut u8) {
            RUNS.fetch_add(1, Ordering::SeqCst);
        }

        RUNS.store(0, Ordering::SeqCst);
        let token = new_box(Some(count));
        assert_eq!(RUNS.load(Ordering::SeqCst), 0);
        // SAFETY: the token is live and freed once.
        assert_eq!(unsafe { kira_rt_native_state_box_free(token) }, 0);
        assert_eq!(RUNS.load(Ordering::SeqCst), 1);
        // SAFETY: the same token, already released.
        let again = unsafe { kira_rt_native_state_box_free(token) };
        assert_eq!(again, NativeStateStatus::UNKNOWN_TOKEN.0);
        assert_eq!(RUNS.load(Ordering::SeqCst), 1);
    }

    /// A retained box survives every release but the last, and the field-drop
    /// leaf runs on that one alone.
    #[test]
    fn a_retained_box_is_freed_by_its_last_release() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        static RUNS: AtomicUsize = AtomicUsize::new(0);

        unsafe extern "C" fn count(_: *mut u8) {
            RUNS.fetch_add(1, Ordering::SeqCst);
        }

        RUNS.store(0, Ordering::SeqCst);
        let token = new_box(Some(count));
        // SAFETY: the token is live.
        assert_eq!(unsafe { kira_rt_native_state_box_retain(token) }, 0);
        // SAFETY: the token is live and has two owners.
        assert_eq!(unsafe { kira_rt_native_state_box_free(token) }, 0);
        assert_eq!(RUNS.load(Ordering::SeqCst), 0);
        assert!(payload(token, TYPE).is_ok());
        // SAFETY: the token is live and has one owner.
        assert_eq!(unsafe { kira_rt_native_state_box_free(token) }, 0);
        assert_eq!(RUNS.load(Ordering::SeqCst), 1);
        // SAFETY: the box is gone, which the header's cleared magic reports.
        let refused = unsafe { kira_rt_native_state_box_retain(token) };
        assert_eq!(refused, NativeStateStatus::UNKNOWN_TOKEN.0);
    }
}
