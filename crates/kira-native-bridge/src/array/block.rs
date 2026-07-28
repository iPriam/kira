//! The item block behind an array handle: its layout and its allocation.
//!
//! A block belongs to exactly one header. Two arrays that share their elements
//! share the *header* that names them ([`super`]), and the write that ends the
//! sharing builds a header and a block together — so nothing here has to count
//! anything.

use std::alloc::{self, Layout};

/// The alignment every item block is allocated to.
///
/// Eight satisfies every element type Kira has — `Int`, `Float`, and every
/// pointer are the widest at eight, and LLVM's ABI size for an element is
/// already rounded up to its own alignment, so element `i` at `i * esize` is
/// aligned whenever the block is.
const ITEMS_ALIGN: usize = 8;

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
/// A block with no elements needs no allocation, and there are two ways to
/// reach one: an empty array (`cap == 0`), or — **unreachable** in Kira, since
/// every element type is at least one byte — a zero-size element. Either way
/// the item pointer stays null, so a null one strictly means an empty block.
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
    match Layout::from_size_align(bytes, ITEMS_ALIGN) {
        Ok(layout) => Some(layout),
        Err(_) => capacity_overflow(),
    }
}

/// Allocates a block for `cap` elements, or null when it would be empty.
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
            block
        }
        None => std::ptr::null_mut(),
    }
}

/// Releases a block, leaving whatever its elements own alone.
///
/// # Safety
/// `items` must be the item pointer of a live block from [`alloc_items`] built
/// for exactly `cap` elements of `esize` bytes.
pub(super) unsafe fn free_items(items: *mut u8, cap: usize, esize: usize) {
    if let Some(layout) = block_layout(cap, esize) {
        // SAFETY: the block came from `alloc` with exactly this layout.
        unsafe { alloc::dealloc(items, layout) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ESIZE: usize = 8;

    #[test]
    fn an_empty_array_is_no_block_at_all() {
        let empty = alloc_items(0, ESIZE);
        assert!(empty.is_null(), "no elements, no allocation");
        assert!(block_layout(0, ESIZE).is_none());
    }

    /// A block that was sized wrong would be freed wrong, which is the failure
    /// an allocator reports long after the mistake.
    #[test]
    fn the_layout_covers_exactly_the_elements() {
        let layout = block_layout(3, ESIZE).expect("three elements need a block");
        assert_eq!(layout.size(), 3 * ESIZE);
        assert_eq!(layout.align(), ITEMS_ALIGN);

        let items = alloc_items(3, ESIZE);
        // SAFETY: a block this test just allocated with these exact sizes.
        unsafe {
            items.cast::<i64>().add(2).write(7);
            assert_eq!(items.cast::<i64>().add(2).read(), 7);
            free_items(items, 3, ESIZE);
        }
    }
}
