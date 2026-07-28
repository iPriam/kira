//! The native array runtime: allocation, bounds checking, growth, and the
//! share-on-read/copy-on-write pair.
//!
//! # ABI
//!
//! A Kira array crosses the boundary as a [`KArray`]: an *opaque owned handle*,
//! one pointer, never an aggregate — the same discipline [`crate::runtime`]'s
//! `KStr` follows, and for the same reason. The header behind it is
//! `#[repr(C)]` because the backend and this crate are compiled separately and
//! have to agree on it.
//!
//! ```text
//!   len     how many elements are live
//!   cap     how many the item block has room for
//!   items   the element storage
//! ```
//!
//! The header is what a value points at, and its address never changes.
//! `xs.append(v)` has to be visible through every path that reaches `xs` — a
//! local, a struct field, an element of an outer array — and all of them hold
//! this address. Growth moves the *items*, never the header.
//!
//! # Copy on write
//!
//! Value semantics say a copy is independent; they do not say when the copying
//! happens. Every handle owns a header of its own, and the *item block* behind
//! it is shared until somebody writes: [`kira_rt_array_clone`] takes a share of
//! the block, and the mutating entry points ([`kira_rt_array_slot_mut`],
//! [`kira_rt_array_push_slot`]) make the block this handle's own first.
//!
//! ```text
//!   handle A ──▶ { len cap items } ─┐
//!                                   ├─▶ [ shares | e0 e1 e2 … ]
//!   handle B ──▶ { len cap items } ─┘
//! ```
//!
//! Reading an array used to cost the whole array — every element, and every
//! string, array and enum inside every element, cloned and then dropped again.
//! A UI frame is mostly such reads, and they were 78% of one.
//!
//! Sharing the block rather than the header is what keeps the paragraph above
//! true: a write makes the block unique by moving `items`, which is a field of
//! the writer's *own* header, so no other holder of a handle has to be found
//! and updated. It also gives each handle its own `len`, so an append after a
//! share lengthens one array and not the other.
//!
//! The count is a plain `usize` in front of the elements, not an atomic: the
//! runtime is single-threaded, as its string and enum heaps already assume.
//!
//! # Why the element type is a size and a callback, not a type
//!
//! These helpers are generic over the element type, so the backend emits one
//! call rather than one copy of this code per array type. Everything here needs
//! to know is:
//!
//! - **how big an element is** (`esize`), which the backend takes from LLVM's
//!   own ABI size for the element type, so the stride is the target's answer
//!   and not a guess made here;
//! - **how to clone and free one**, which arrives as a function pointer the
//!   backend emits — a two-instruction leaf for a `String`, a walk for a
//!   struct. A null callback means the element owns nothing, and then the flat
//!   `memcpy` a clone starts with is already the whole job.
//!
//! That split is what keeps the *loop* in Rust, where it is ordinary code, and
//! leaves LLVM emitting only leaves.
//!
//! # Ownership
//!
//! Affine, mirroring the VM's heap: reading an array copies it
//! ([`kira_rt_array_clone`]), and a local leaving scope or being overwritten
//! frees it ([`kira_rt_array_free`]). A well-formed program frees every
//! allocation exactly once — the same guarantee the VM proves with its heap
//! accounting. A shared block is freed by whichever handle gives up the last
//! share, so the elements it owns are still released exactly once.
//!
//! Every symbol is `extern "C"` with a `kira_rt_` prefix and a fixed signature.
//! These names are a wire contract with the backend's lowering and are
//! append-only: never rename one or change a signature in place.

mod block;

use std::alloc::Layout;

use crate::pool::SharedPool;
use block::{alloc_items, drop_share, free_items, share_count, take_share};

/// The free list array headers are handed out from.
///
/// A header is allocated per array *copy*, which after the sharing above is the
/// only allocation a copy makes at all — and the most frequent one a program
/// makes. See [`crate::pool`], which also states what a `static` free list
/// assumes and why the share count above assumes it already.
static HEADERS: SharedPool = SharedPool::new(Layout::new::<KiraArray>());

/// Takes a header from the free list and fills it in.
fn new_header(len: usize, cap: usize, items: *mut u8) -> KArray {
    let header = HEADERS.alloc().cast::<KiraArray>();
    // SAFETY: the pool hands back a block of exactly this layout, and every
    // field is written before anything reads one.
    unsafe {
        header.write(KiraArray { len, cap, items });
    }
    header
}

/// Returns a header to the free list.
///
/// # Safety
/// `header` must be a live header from [`new_header`], not returned already,
/// and whatever it owned must have been released.
unsafe fn free_header(header: KArray) {
    // SAFETY: the caller vouches the header is live and finished with; a header
    // owns nothing itself, so there is nothing to drop before it goes.
    unsafe { HEADERS.free(header.cast::<u8>()) };
}

/// A Kira array at the native ABI: an opaque owned handle.
pub type KArray = *mut KiraArray;

/// Clones one element: read from `src`, write an independent copy to `dst`.
///
/// `dst` already holds a bitwise copy of `src` when this is called, so an
/// implementation replaces exactly the parts that own storage.
pub type ElemClone = unsafe extern "C" fn(src: *const u8, dst: *mut u8);

/// Frees whatever the element at `at` owns. Does not free the slot itself.
pub type ElemFree = unsafe extern "C" fn(at: *mut u8);

/// The heap object behind a [`KArray`].
///
/// `#[repr(C)]` because the backend GEPs into it: the two are compiled
/// separately, so the layout is a contract rather than an implementation
/// detail.
#[repr(C)]
pub struct KiraArray {
    /// How many elements are live.
    pub len: usize,
    /// How many the item block has room for.
    pub cap: usize,
    /// The element storage; null when `cap` is zero.
    pub items: *mut u8,
}

/// Makes a handle's item block its own, so a write through it is seen by
/// nothing else.
///
/// One share is the common case and costs a load and a compare. Otherwise the
/// block is duplicated — flat first, then `element` over each live element,
/// which is exactly the deep copy [`kira_rt_array_clone`] used to do eagerly —
/// and the original block loses this handle's share.
///
/// # Safety
/// `header` must be a live array header; `esize` must be the element size it
/// was built with; and `element`, when given, must clone exactly one element of
/// that size.
unsafe fn make_unique(header: &mut KiraArray, esize: usize, element: Option<ElemClone>) {
    let shared = header.items;
    // SAFETY: `shared` belongs to a live header.
    if unsafe { share_count(shared) } == 1 {
        return;
    }
    let fresh = alloc_items(header.cap, esize);
    // SAFETY: a block with more than one share is non-null and holds at least
    // `len * esize` bytes, and `fresh` is a new allocation of the same capacity
    // that cannot overlap it.
    unsafe {
        std::ptr::copy_nonoverlapping(shared, fresh, header.len * esize);
        if let Some(clone) = element {
            for at in 0..header.len {
                clone(shared.add(at * esize), fresh.add(at * esize));
            }
        }
        drop_share(shared);
    }
    header.items = fresh;
}

/// Allocates an array of `count` elements, with `len == cap == count`.
///
/// A literal's array is full the moment it exists; an empty one gets no item
/// block at all, and the first append is what buys one.
#[unsafe(no_mangle)]
pub extern "C" fn kira_rt_array_new(count: usize, esize: usize) -> KArray {
    new_header(count, count, alloc_items(count, esize))
}

/// The number of elements in an array.
///
/// # Safety
/// `array` must be a live handle from this runtime.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_rt_array_len(array: KArray) -> i64 {
    if array.is_null() {
        return 0;
    }
    // SAFETY: the caller guarantees a live handle.
    let len = unsafe { (*array).len };
    // An array of more than `i64::MAX` elements cannot be allocated on any
    // target this runs on, so the conversion is total in practice; it is still
    // written as a saturating one rather than an unwrap, because a runtime
    // never gets to end its caller's process.
    i64::try_from(len).unwrap_or(i64::MAX)
}

/// Makes an array's item block its own, so its elements can be moved out of it.
///
/// The runtime's own way in to what [`kira_rt_array_slot_mut`] does before it
/// hands out a writable element. Taking an element *away* from a shared block
/// is a write like any other: without this, the handle left holding the block
/// would read storage that had been moved out from under it.
///
/// # Safety
/// `array` must be a live handle from this runtime; `esize` must be the element
/// size it was built with; and `element`, when given, must clone exactly one
/// element of that size.
pub(crate) unsafe fn make_array_unique(array: KArray, esize: usize, element: Option<ElemClone>) {
    if array.is_null() {
        return;
    }
    // SAFETY: the caller's promises are exactly `make_unique`'s.
    unsafe { make_unique(&mut *array, esize, element) };
}

/// The address of element `index` **to read**, bounds-checked.
///
/// One of the two places a bounds check lives — [`kira_rt_array_slot_mut`] is
/// the other — so no element access can forget one. A negative index and one
/// past the end are **different traps**, because they are different mistakes —
/// the VM draws the same line.
///
/// The block may be shared, so nothing may be written through this address; a
/// write goes through [`kira_rt_array_slot_mut`], which is what makes the
/// sharing invisible.
///
/// # Safety
/// `array` must be a live handle from this runtime, and `esize` must be the
/// element size it was built with.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_rt_array_slot(array: KArray, index: i64, esize: usize) -> *mut u8 {
    if index < 0 {
        kira_rt_trap_index_negative();
    }
    if array.is_null() {
        kira_rt_trap_index_out_of_bounds();
    }
    // SAFETY: the caller guarantees a live handle.
    let header = unsafe { &*array };
    let at = index as usize;
    if at >= header.len {
        kira_rt_trap_index_out_of_bounds();
    }
    // SAFETY: `at < len <= cap`, so the offset lands inside the item block.
    unsafe { header.items.add(at * esize) }
}

/// The address of element `index` **to write**, bounds-checked, in a block this
/// handle owns alone.
///
/// The bounds check comes first, so a trapping index never allocates: the
/// program is ending either way, and a copy made on the way out would be one
/// the trap message has to be read past.
///
/// # Safety
/// `array` must be a live handle from this runtime; `esize` must be the element
/// size it was built with; and `element`, when given, must clone exactly one
/// element of that size.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_rt_array_slot_mut(
    array: KArray,
    index: i64,
    esize: usize,
    element: Option<ElemClone>,
) -> *mut u8 {
    if index < 0 {
        kira_rt_trap_index_negative();
    }
    if array.is_null() {
        kira_rt_trap_index_out_of_bounds();
    }
    // SAFETY: the caller guarantees a live handle.
    let header = unsafe { &mut *array };
    let at = index as usize;
    if at >= header.len {
        kira_rt_trap_index_out_of_bounds();
    }
    // SAFETY: the caller's promises are exactly this function's.
    unsafe {
        make_unique(header, esize, element);
        // `at < len <= cap`, so the offset lands inside the item block.
        header.items.add(at * esize)
    }
}

/// Makes room for one more element in a block this handle owns alone, and
/// returns where the element goes.
///
/// Capacity doubles (from one, when there is none), so a run of appends copies
/// O(n) elements in total rather than O(n²).
///
/// # Safety
/// `array` must be a live handle from this runtime; `esize` must be the element
/// size it was built with; and `element`, when given, must clone exactly one
/// element of that size.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_rt_array_push_slot(
    array: KArray,
    esize: usize,
    element: Option<ElemClone>,
) -> *mut u8 {
    // SAFETY: the caller guarantees a live handle and a matching callback.
    let header = unsafe { &mut *array };
    // Appending is a write, so the block becomes this handle's before anything
    // lands in it — growth included, which would otherwise abandon the shared
    // block while another handle still counted on this one holding a share.
    // SAFETY: as above.
    unsafe { make_unique(header, esize, element) };
    if header.len == header.cap {
        let grown = if header.cap == 0 { 1 } else { header.cap * 2 };
        let fresh = alloc_items(grown, esize);
        if !header.items.is_null() {
            // SAFETY: both blocks hold at least `len * esize` bytes and cannot
            // overlap — `fresh` is a new allocation — and the old block is this
            // handle's alone, so nothing else is reading it.
            unsafe {
                std::ptr::copy_nonoverlapping(header.items, fresh, header.len * esize);
                free_items(header.items, header.cap, esize);
            }
        }
        header.items = fresh;
        header.cap = grown;
    }
    let at = header.len;
    header.len += 1;
    // SAFETY: the block now has room for at least `at + 1` elements.
    unsafe { header.items.add(at * esize) }
}

/// Produces an independent copy of an array: a fresh header taking a share of
/// the same item block.
///
/// Independent is a promise about what a reader can observe, not about where
/// the bytes live. Nothing can be written through the copy without
/// [`kira_rt_array_slot_mut`] or [`kira_rt_array_push_slot`] first making the
/// block that handle's own, so the two arrays are indistinguishable from two
/// deep copies — at the cost of one 24-byte header rather than the whole array
/// and everything its elements own.
///
/// # Safety
/// `array` must be null or a live handle from this runtime.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_rt_array_clone(array: KArray) -> KArray {
    if array.is_null() {
        return kira_rt_array_new(0, 0);
    }
    // SAFETY: the caller guarantees a live handle.
    let source = unsafe { &*array };
    // SAFETY: a live header's item pointer is null or names a live block, and
    // this new handle joins its count.
    unsafe { take_share(source.items) };
    new_header(source.len, source.cap, source.items)
}

/// Frees an array: always its header, and the item block and whatever its
/// elements own once no handle is left holding it.
///
/// `element` is null when the elements own nothing, and then only the block and
/// the header go.
///
/// # Safety
/// `array` must be null or a live handle from this runtime, not already freed;
/// `esize` must be the element size it was built with; and `element`, when
/// given, must free exactly one element of that size.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_rt_array_free(
    array: KArray,
    esize: usize,
    element: Option<ElemFree>,
) {
    if array.is_null() {
        return;
    }
    // SAFETY: the caller guarantees a live handle it is giving up. The header
    // goes back to the pool either way; what it points at may not.
    let (items, len, cap) = unsafe { ((*array).items, (*array).len, (*array).cap) };
    // SAFETY: the caller gave up this handle and nothing reads it again.
    unsafe { free_header(array) };
    if items.is_null() {
        return;
    }
    // SAFETY: a non-null item pointer names a live block.
    let count = unsafe { share_count(items) };
    if count > 1 {
        // Another handle still reads these elements, so nothing they own is
        // released here — only this handle's claim on them.
        // SAFETY: as above, and the block is shared.
        unsafe { drop_share(items) };
        return;
    }
    if let Some(free) = element {
        for at in 0..len {
            // SAFETY: the offset lands inside a block of `len` elements.
            unsafe { free(items.add(at * esize)) };
        }
    }
    // SAFETY: the last share was this one, so the block is unheld.
    unsafe { free_items(items, cap, esize) };
}

/// Reports an out-of-range array index and exits with a failure code, mirroring
/// the VM's `IndexOutOfBounds` trap: no further output, non-zero exit.
#[unsafe(no_mangle)]
pub extern "C" fn kira_rt_trap_index_out_of_bounds() -> ! {
    eprintln!("kira: runtime trap: array index is out of bounds");
    crate::runtime::print_trap_backtrace();
    std::process::exit(1);
}

/// Reports a negative array index, mirroring the VM's `NegativeIndex` trap.
///
/// A different trap from [`kira_rt_trap_index_out_of_bounds`], because it is a
/// different mistake: a computation that went wrong rather than a length that
/// was misjudged.
#[unsafe(no_mangle)]
pub extern "C" fn kira_rt_trap_index_negative() -> ! {
    eprintln!("kira: runtime trap: array index is negative");
    crate::runtime::print_trap_backtrace();
    std::process::exit(1);
}

#[cfg(test)]
mod tests {
    use super::*;

    const ESIZE: usize = 8;

    /// Reads element `at` of an `Int` array.
    unsafe fn read(array: KArray, at: usize) -> i64 {
        // SAFETY: the tests only read elements they wrote.
        unsafe { *(*array).items.add(at * ESIZE).cast::<i64>() }
    }

    #[test]
    fn a_new_array_is_full_and_an_empty_one_owns_no_block() {
        let empty = kira_rt_array_new(0, ESIZE);
        // SAFETY: a handle this test just built.
        unsafe {
            assert_eq!(kira_rt_array_len(empty), 0);
            assert!((*empty).items.is_null(), "nothing to allocate for zero");
            kira_rt_array_free(empty, ESIZE, None);
        }

        let three = kira_rt_array_new(3, ESIZE);
        // SAFETY: a handle this test just built.
        unsafe {
            assert_eq!(kira_rt_array_len(three), 3);
            assert_eq!((*three).cap, 3);
            kira_rt_array_free(three, ESIZE, None);
        }
    }

    #[test]
    fn appending_grows_and_keeps_what_was_there() {
        let array = kira_rt_array_new(0, ESIZE);
        // SAFETY: a handle this test just built, with a matching element size.
        unsafe {
            for value in 0..10i64 {
                let slot = kira_rt_array_push_slot(array, ESIZE, None);
                *slot.cast::<i64>() = value;
            }
            assert_eq!(kira_rt_array_len(array), 10);
            for value in 0..10i64 {
                assert_eq!(read(array, value as usize), value, "growth kept element");
            }
            // Doubling from one: 1, 2, 4, 8, 16 — never exactly the length.
            assert_eq!((*array).cap, 16);
            kira_rt_array_free(array, ESIZE, None);
        }
    }

    #[test]
    fn the_header_address_survives_growth() {
        let array = kira_rt_array_new(0, ESIZE);
        // SAFETY: a handle this test just built.
        unsafe {
            let before = array;
            for value in 0..64i64 {
                *kira_rt_array_push_slot(array, ESIZE, None).cast::<i64>() = value;
            }
            // The whole design: a holder of the handle still sees the growth.
            assert_eq!(before, array);
            assert_eq!(read(array, 63), 63);
            kira_rt_array_free(array, ESIZE, None);
        }
    }

    #[test]
    fn a_copy_shares_the_block_until_one_of_them_is_written() {
        let array = kira_rt_array_new(2, ESIZE);
        // SAFETY: a handle this test just built.
        unsafe {
            *(*array).items.cast::<i64>() = 1;
            *(*array).items.add(ESIZE).cast::<i64>() = 2;

            let copy = kira_rt_array_clone(array);
            assert_eq!((*array).items, (*copy).items, "reading copies nothing");
            assert_eq!(share_count((*array).items), 2);

            // The write is what buys the copy its own block, and the original
            // is what it was before — which is the whole of value semantics.
            *kira_rt_array_slot_mut(copy, 0, ESIZE, None).cast::<i64>() = 99;
            assert_ne!((*array).items, (*copy).items, "the block is its own now");
            assert_eq!(share_count((*array).items), 1, "the share came back");
            assert_eq!(read(array, 0), 1, "the original is untouched");
            assert_eq!(read(copy, 0), 99);
            assert_eq!(read(copy, 1), 2, "the rest of the block came along");

            kira_rt_array_free(array, ESIZE, None);
            kira_rt_array_free(copy, ESIZE, None);
        }
    }

    /// A read through the *original* is the mirror case: the copy is the one
    /// left holding the block, so the write has to reach the copy's own.
    #[test]
    fn writing_the_original_leaves_a_copy_alone() {
        let array = kira_rt_array_new(1, ESIZE);
        // SAFETY: a handle this test just built.
        unsafe {
            *(*array).items.cast::<i64>() = 7;
            let copy = kira_rt_array_clone(array);
            *kira_rt_array_slot_mut(array, 0, ESIZE, None).cast::<i64>() = 8;
            assert_eq!(read(array, 0), 8);
            assert_eq!(read(copy, 0), 7);
            kira_rt_array_free(array, ESIZE, None);
            kira_rt_array_free(copy, ESIZE, None);
        }
    }

    /// Appending is a write like any other, and it is the one that would
    /// otherwise abandon a block another handle still counts on.
    #[test]
    fn appending_to_a_copy_leaves_the_original_at_its_length() {
        let array = kira_rt_array_new(2, ESIZE);
        // SAFETY: a handle this test just built.
        unsafe {
            let copy = kira_rt_array_clone(array);
            *kira_rt_array_push_slot(copy, ESIZE, None).cast::<i64>() = 5;
            assert_eq!(kira_rt_array_len(copy), 3);
            assert_eq!(
                kira_rt_array_len(array),
                2,
                "the original is its own length"
            );
            assert_eq!(share_count((*array).items), 1);
            kira_rt_array_free(array, ESIZE, None);
            kira_rt_array_free(copy, ESIZE, None);
        }
    }

    #[test]
    fn a_copy_of_an_empty_array_is_an_empty_array() {
        let array = kira_rt_array_new(0, ESIZE);
        // SAFETY: a handle this test just built.
        unsafe {
            let copy = kira_rt_array_clone(array);
            assert_eq!(kira_rt_array_len(copy), 0);
            kira_rt_array_free(array, ESIZE, None);
            kira_rt_array_free(copy, ESIZE, None);
        }
    }

    /// The element callback is what makes the deferred copy deep. This stands
    /// in for the leaf the backend emits for a `String` element: without it the
    /// two arrays would end up holding one handle between them.
    #[test]
    fn making_a_block_unique_runs_the_element_callback_over_every_element() {
        unsafe extern "C" fn bump(src: *const u8, dst: *mut u8) {
            // SAFETY: the runtime hands both pointers at element slots.
            unsafe { *dst.cast::<i64>() = *src.cast::<i64>() + 100 };
        }
        let array = kira_rt_array_new(3, ESIZE);
        // SAFETY: a handle this test just built.
        unsafe {
            for at in 0..3usize {
                *(*array).items.add(at * ESIZE).cast::<i64>() = at as i64;
            }
            let copy = kira_rt_array_clone(array);
            // Writing element 2 copies the block, so elements 0 and 1 are
            // cloned by the callback rather than left aliasing the original.
            *kira_rt_array_slot_mut(copy, 2, ESIZE, Some(bump)).cast::<i64>() = 42;
            assert_eq!(read(copy, 0), 100);
            assert_eq!(read(copy, 1), 101);
            assert_eq!(read(copy, 2), 42);
            for at in 0..3usize {
                assert_eq!(read(array, at), at as i64, "the original is untouched");
            }
            kira_rt_array_free(array, ESIZE, None);
            kira_rt_array_free(copy, ESIZE, None);
        }
    }

    /// A free runs the element callback exactly once per live element — not per
    /// slot of capacity, which is what a leak or a double free would look like.
    #[test]
    fn a_free_runs_the_element_callback_once_per_live_element() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static FREED: AtomicUsize = AtomicUsize::new(0);
        unsafe extern "C" fn count(_at: *mut u8) {
            FREED.fetch_add(1, Ordering::Relaxed);
        }
        let array = kira_rt_array_new(0, ESIZE);
        // SAFETY: a handle this test just built.
        unsafe {
            for value in 0..5i64 {
                *kira_rt_array_push_slot(array, ESIZE, None).cast::<i64>() = value;
            }
            // Capacity is 8 by now, but only 5 elements are live.
            assert_eq!((*array).cap, 8);
            kira_rt_array_free(array, ESIZE, Some(count));
        }
        assert_eq!(FREED.load(Ordering::Relaxed), 5);
    }

    /// What the elements own is released once, by whichever handle is last —
    /// never once per handle, which is the double free sharing invites.
    #[test]
    fn a_shared_block_frees_its_elements_once_and_only_at_the_end() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static FREED: AtomicUsize = AtomicUsize::new(0);
        unsafe extern "C" fn count(_at: *mut u8) {
            FREED.fetch_add(1, Ordering::Relaxed);
        }
        let array = kira_rt_array_new(3, ESIZE);
        // SAFETY: a handle this test just built.
        unsafe {
            let copy = kira_rt_array_clone(array);
            kira_rt_array_free(array, ESIZE, Some(count));
            assert_eq!(
                FREED.load(Ordering::Relaxed),
                0,
                "the copy still reads these elements"
            );
            kira_rt_array_free(copy, ESIZE, Some(count));
        }
        assert_eq!(FREED.load(Ordering::Relaxed), 3);
    }

    #[test]
    fn a_null_handle_is_a_free_that_does_nothing() {
        // SAFETY: null is explicitly allowed.
        unsafe {
            kira_rt_array_free(std::ptr::null_mut(), ESIZE, None);
            assert_eq!(kira_rt_array_len(std::ptr::null_mut()), 0);
            let copy = kira_rt_array_clone(std::ptr::null_mut());
            assert_eq!(kira_rt_array_len(copy), 0, "a copy of nothing is empty");
            kira_rt_array_free(copy, ESIZE, None);
        }
    }

    /// The backend GEPs into this header, so its shape is a contract with code
    /// compiled separately from this crate — and the share count deliberately
    /// is *not* in it, so that a copy gets a header of its own.
    #[test]
    fn the_header_layout_is_three_words() {
        assert_eq!(size_of::<KiraArray>(), 3 * size_of::<usize>());
        assert_eq!(align_of::<KiraArray>(), align_of::<usize>());
        let header = KiraArray {
            len: 0,
            cap: 0,
            items: std::ptr::null_mut(),
        };
        let base = std::ptr::from_ref(&header).cast::<u8>();
        // SAFETY: every field belongs to `header`, which outlives the reads.
        unsafe {
            assert_eq!(
                std::ptr::from_ref(&header.len)
                    .cast::<u8>()
                    .offset_from(base),
                0
            );
            assert_eq!(
                std::ptr::from_ref(&header.cap)
                    .cast::<u8>()
                    .offset_from(base),
                size_of::<usize>() as isize
            );
            assert_eq!(
                std::ptr::from_ref(&header.items)
                    .cast::<u8>()
                    .offset_from(base),
                2 * size_of::<usize>() as isize
            );
        }
    }
}
