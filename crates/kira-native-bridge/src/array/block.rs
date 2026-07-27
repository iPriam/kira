//! The item block behind an array handle: its layout, its allocation, and the
//! share count that lets several handles hold one block.
//!
//! ```text
//!   block ─▶ [ shares : usize ][ padding to ITEMS_ALIGN ][ e0 e1 e2 … ]
//!            └──────── SHARE_PREFIX ────────┘
//!                                            ▲
//!                                            └── what a header's `items` names
//! ```
//!
//! The count sits *in front of* the elements rather than in the header, because
//! the header is per handle and the block is what is shared. Nothing outside
//! this module knows the prefix exists: a block is handed out and taken back as
//! its item pointer, which is the address element zero lives at.

use std::alloc::{self, Layout};

/// The alignment every item block is allocated to.
///
/// Eight satisfies every element type Kira has — `Int`, `Float`, and every
/// pointer are the widest at eight, and LLVM's ABI size for an element is
/// already rounded up to its own alignment, so element `i` at `i * esize` is
/// aligned whenever the block is.
const ITEMS_ALIGN: usize = 8;

/// The bytes reserved in front of the first element for the share count.
///
/// A multiple of [`ITEMS_ALIGN`], so element `i` sits at `items + i * esize`
/// with the alignment it had before the count existed — and wide enough for a
/// `usize` on every target this runs on, including a 32-bit wasm one, which
/// `the_share_count_fits_its_prefix` pins.
const SHARE_PREFIX: usize = 8;

/// Aborts on a capacity whose byte size cannot be represented, the same
/// response `Vec` gives — and unreachable for any array a Kira program can
/// build, since a live array never exceeds the address space it already
/// occupies.
#[cold]
fn capacity_overflow() -> ! {
    std::process::abort();
}

/// The layout of a block for `cap` elements, or `None` when it holds none.
///
/// A block with no elements needs no allocation — and no share count, since
/// there is nothing to share — and there are two ways to reach one: an empty
/// array (`cap == 0`), or — **unreachable** in Kira, since every element type
/// is at least one byte — a zero-size element. Either way the item pointer
/// stays null, so a null one strictly means an empty block.
///
/// A capacity whose byte size overflows the address space is a different case:
/// it is not a "no block" request but an impossible one, so it aborts rather
/// than returning a `None` a caller would misread as empty and then write
/// through a null pointer.
fn block_layout(cap: usize, esize: usize) -> Option<Layout> {
    let bytes = match cap.checked_mul(esize) {
        Some(bytes) => bytes,
        None => capacity_overflow(),
    };
    if bytes == 0 {
        return None;
    }
    let total = match bytes.checked_add(SHARE_PREFIX) {
        Some(total) => total,
        None => capacity_overflow(),
    };
    match Layout::from_size_align(total, ITEMS_ALIGN) {
        Ok(layout) => Some(layout),
        Err(_) => capacity_overflow(),
    }
}

/// The address of a block's share count.
///
/// # Safety
/// `items` must be the non-null item pointer of a block from [`alloc_items`].
unsafe fn shares(items: *mut u8) -> *mut usize {
    // SAFETY: the caller guarantees a live block, whose allocation starts
    // `SHARE_PREFIX` bytes before its items and begins with this count.
    unsafe { items.sub(SHARE_PREFIX).cast::<usize>() }
}

/// How many handles hold this block, or one when there is no block to hold.
///
/// # Safety
/// `items` must be null or the item pointer of a block from [`alloc_items`].
pub(super) unsafe fn share_count(items: *mut u8) -> usize {
    if items.is_null() {
        return 1;
    }
    // SAFETY: the caller guarantees a live block.
    unsafe { *shares(items) }
}

/// Records one more handle holding this block.
///
/// # Safety
/// `items` must be null or the item pointer of a block from [`alloc_items`].
pub(super) unsafe fn take_share(items: *mut u8) {
    if items.is_null() {
        return;
    }
    // SAFETY: the caller guarantees a live block. The count cannot wrap: it
    // rises by one per live handle, and a handle is a `Box` of its own.
    unsafe { *shares(items) += 1 };
}

/// Gives up one handle's hold on this block, leaving it allocated.
///
/// The caller has already decided the block outlives this handle — [`free_items`]
/// is the other half, for the handle that finds itself last.
///
/// # Safety
/// `items` must be the item pointer of a block from [`alloc_items`] with more
/// than one share.
pub(super) unsafe fn drop_share(items: *mut u8) {
    // SAFETY: the caller guarantees a live, shared block.
    unsafe { *shares(items) -= 1 };
}

/// Allocates a block for `cap` elements held by one handle, or null when it
/// would be empty.
///
/// A failed allocation aborts through [`alloc::handle_alloc_error`] — the same
/// response `Box`/`Vec` give — rather than returning the null `alloc` hands back
/// on failure, which the callers would then write through.
pub(super) fn alloc_items(cap: usize, esize: usize) -> *mut u8 {
    match block_layout(cap, esize) {
        Some(layout) => {
            // SAFETY: the layout is non-zero-sized, which is what `alloc`
            // requires.
            let block = unsafe { alloc::alloc(layout) };
            if block.is_null() {
                alloc::handle_alloc_error(layout);
            }
            // SAFETY: the block is at least `SHARE_PREFIX` bytes long and
            // aligned to `ITEMS_ALIGN`, which a `usize` satisfies.
            unsafe {
                block.cast::<usize>().write(1);
                block.add(SHARE_PREFIX)
            }
        }
        None => std::ptr::null_mut(),
    }
}

/// Releases a block, leaving whatever its elements own alone.
///
/// # Safety
/// `items` must be the item pointer of a live block from [`alloc_items`] built
/// for exactly `cap` elements of `esize` bytes, held by nobody else.
pub(super) unsafe fn free_items(items: *mut u8, cap: usize, esize: usize) {
    if let Some(layout) = block_layout(cap, esize) {
        // SAFETY: the block came from `alloc` with exactly this layout, and it
        // starts `SHARE_PREFIX` bytes before the items.
        unsafe { alloc::dealloc(items.sub(SHARE_PREFIX), layout) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ESIZE: usize = 8;

    /// The prefix has to hold the count on every target, and a 32-bit one has a
    /// narrower `usize` than the host these tests run on.
    #[test]
    fn the_share_count_fits_its_prefix() {
        assert!(size_of::<usize>() <= SHARE_PREFIX);
        assert!(align_of::<usize>() <= ITEMS_ALIGN);
        assert_eq!(SHARE_PREFIX % ITEMS_ALIGN, 0, "elements stay aligned");
    }

    #[test]
    fn a_fresh_block_is_held_once_and_an_empty_one_is_no_block() {
        let items = alloc_items(4, ESIZE);
        // SAFETY: a block this test just allocated.
        unsafe {
            assert_eq!(share_count(items), 1);
            free_items(items, 4, ESIZE);
        }

        let empty = alloc_items(0, ESIZE);
        assert!(empty.is_null(), "no elements, no allocation");
        // A block nobody allocated is held by exactly the one handle that has
        // it, so a caller asking whether to copy gets "no".
        // SAFETY: null is explicitly allowed.
        assert_eq!(unsafe { share_count(empty) }, 1);
    }

    #[test]
    fn shares_are_taken_and_given_back() {
        let items = alloc_items(2, ESIZE);
        // SAFETY: a block this test just allocated, with matching sizes.
        unsafe {
            take_share(items);
            take_share(items);
            assert_eq!(share_count(items), 3);
            drop_share(items);
            assert_eq!(share_count(items), 2);
            drop_share(items);
            assert_eq!(share_count(items), 1);
            free_items(items, 2, ESIZE);
        }
    }

    /// The layout covers the prefix as well as the elements; a block that
    /// forgot it would be freed at the wrong size.
    #[test]
    fn the_layout_covers_the_prefix_and_the_elements() {
        let layout = block_layout(3, ESIZE).expect("three elements need a block");
        assert_eq!(layout.size(), SHARE_PREFIX + 3 * ESIZE);
        assert_eq!(layout.align(), ITEMS_ALIGN);
        assert!(block_layout(0, ESIZE).is_none(), "no elements, no block");
    }
}
