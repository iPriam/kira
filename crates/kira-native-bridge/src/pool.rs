//! A free list for the fixed-size boxes the runtime hands out.
//!
//! An array header and an enum box are the two objects a Kira program allocates
//! most: one per array copy, one per enum construction. A UI frame that rebuilds
//! its view tree makes thousands of each and releases them again in the same
//! frame, and `malloc`/`free` were **36%** of a Project Matter frame once the
//! copying itself was gone.
//!
//! Both are one small, fixed size with no destructor, which is exactly the shape
//! a free list serves: a release pushes the block onto a list and an allocation
//! pops it, so the common case is two loads and two stores and touches memory
//! that is still warm from the last time it was used.
//!
//! # Bounded on purpose
//!
//! A pooled block is never returned to the allocator, so an unbounded list
//! would let one burst of allocation hold its memory for the life of the
//! process. Past [`Pool::CAPACITY`] blocks a release frees instead of pooling,
//! which bounds what a pool retains to its capacity and leaves a long-running
//! program's footprint where its live data puts it.
//!
//! # One list per thread
//!
//! Each thread keeps its own, so no lock and no atomic is involved. A block
//! allocated on one thread and released on another simply joins the releasing
//! thread's list; every block comes from the global allocator and is only ever
//! returned to it under the same layout, so which list it waits on is a matter
//! of locality rather than correctness.

use std::alloc::{self, Layout};
use std::cell::Cell;

/// A free list of same-sized blocks.
///
/// The list threads through the blocks themselves: a free block's first word is
/// the next one's address, which is why a block has to be at least pointer-sized
/// — [`Pool::new`] is where that is checked.
pub(crate) struct Pool {
    /// The most recently released block, or null when the list is empty.
    head: Cell<*mut u8>,
    /// How many blocks the list holds.
    len: Cell<usize>,
    /// The size and alignment every block in this list has.
    layout: Layout,
}

impl Pool {
    /// How many blocks a list keeps before a release frees instead.
    ///
    /// Enough that a frame's worth of churn never reaches the allocator, small
    /// enough that the memory a pool retains is a rounding error: at 32 bytes a
    /// block, a full list is 128 KB.
    const CAPACITY: usize = 4096;

    /// A pool of blocks with `layout`.
    ///
    /// `layout` must be at least pointer-sized and pointer-aligned, since a free
    /// block stores the next one's address in its first word. Both of this
    /// crate's pools are, and `every_pool_can_hold_a_link` says so.
    pub(crate) const fn new(layout: Layout) -> Self {
        Self {
            head: Cell::new(std::ptr::null_mut()),
            len: Cell::new(0),
            layout,
        }
    }

    /// A block of this pool's layout, reused when the list has one.
    ///
    /// The contents are whatever the last holder left behind, exactly as
    /// [`alloc::alloc`]'s are: a caller initializes every field.
    pub(crate) fn alloc(&self) -> *mut u8 {
        let block = self.head.get();
        if block.is_null() {
            // SAFETY: both pools have a non-zero-sized layout.
            let fresh = unsafe { alloc::alloc(self.layout) };
            if fresh.is_null() {
                alloc::handle_alloc_error(self.layout);
            }
            return fresh;
        }
        // SAFETY: a pooled block is at least pointer-sized and pointer-aligned,
        // and its first word is the next block's address, written by `free`.
        self.head.set(unsafe { *block.cast::<*mut u8>() });
        self.len.set(self.len.get() - 1);
        block
    }

    /// Takes a block back for reuse, or releases it when the list is full.
    ///
    /// # Safety
    /// `block` must be a live block this pool's layout allocated — from
    /// [`Pool::alloc`] or from [`alloc::alloc`] with the same layout — and must
    /// not be used again.
    pub(crate) unsafe fn free(&self, block: *mut u8) {
        if self.len.get() >= Self::CAPACITY {
            // SAFETY: the caller vouches for a block of exactly this layout.
            unsafe { alloc::dealloc(block, self.layout) };
            return;
        }
        // SAFETY: the block is at least pointer-sized and pointer-aligned, and
        // nothing reads it again until `alloc` hands it back.
        unsafe { *block.cast::<*mut u8>() = self.head.get() };
        self.head.set(block);
        self.len.set(self.len.get() + 1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stand-in for the two boxes the runtime pools.
    #[repr(C)]
    struct Block {
        a: usize,
        b: usize,
        c: usize,
    }

    fn pool() -> Pool {
        Pool::new(Layout::new::<Block>())
    }

    /// A free block's first word is the link, so a block narrower than a
    /// pointer would overwrite the block next to it.
    #[test]
    fn every_pool_can_hold_a_link() {
        for layout in [
            Layout::new::<crate::array::KiraArray>(),
            Layout::new::<crate::enums::KiraEnum>(),
        ] {
            assert!(layout.size() >= size_of::<*mut u8>());
            assert!(layout.align() >= align_of::<*mut u8>());
        }
    }

    #[test]
    fn a_released_block_is_the_next_one_handed_out() {
        let pool = pool();
        let first = pool.alloc();
        // SAFETY: a block this pool just handed out, used no further.
        unsafe { pool.free(first) };
        assert_eq!(pool.alloc(), first, "the list is last in, first out");
    }

    #[test]
    fn blocks_come_back_in_reverse_order_and_the_list_empties() {
        let pool = pool();
        let blocks = [pool.alloc(), pool.alloc(), pool.alloc()];
        for block in blocks {
            // SAFETY: each block is live and released once.
            unsafe { pool.free(block) };
        }
        assert_eq!(pool.len.get(), 3);
        for block in blocks.iter().rev() {
            assert_eq!(pool.alloc(), *block);
        }
        assert_eq!(pool.len.get(), 0);
        assert!(pool.head.get().is_null());
        for block in blocks {
            // SAFETY: each block is live and released once.
            unsafe { pool.free(block) };
        }
    }

    /// Past the cap a release frees, so a burst cannot make a pool hold memory
    /// for the life of the process.
    #[test]
    fn a_full_list_stops_growing() {
        let pool = pool();
        let blocks: Vec<*mut u8> = (0..Pool::CAPACITY + 8).map(|_| pool.alloc()).collect();
        for block in &blocks {
            // SAFETY: each block is live and released once; past the cap this
            // hands it to the allocator rather than the list.
            unsafe { pool.free(*block) };
        }
        assert_eq!(pool.len.get(), Pool::CAPACITY);
        // Drain, so the test leaves nothing allocated behind it.
        for _ in 0..Pool::CAPACITY {
            let block = pool.alloc();
            // SAFETY: a block this pool just handed out.
            unsafe { alloc::dealloc(block, Layout::new::<Block>()) };
        }
    }
}
