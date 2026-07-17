//! The native array runtime: allocation, bounds checking, growth, and the
//! affine clone/free pair.
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
//! Affine, mirroring the VM's heap: reading an array clones it
//! ([`kira_rt_array_clone`]), and a local leaving scope or being overwritten
//! frees it ([`kira_rt_array_free`]). A well-formed program frees every
//! allocation exactly once — the same guarantee the VM proves with its heap
//! accounting.
//!
//! Every symbol is `extern "C"` with a `kira_rt_` prefix and a fixed signature.
//! These names are a wire contract with the backend's lowering and are
//! append-only: never rename one or change a signature in place.

use std::alloc::{self, Layout};

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

/// The layout of an item block, or `None` when it would hold no bytes.
///
/// A zero-byte block needs no allocation, and there are two ways to reach one:
/// an empty array (`cap == 0`), or — **unreachable** in Kira, since every
/// element type is at least one byte — a zero-size element. Either way `items`
/// stays null, so a null `items` strictly means an empty block.
///
/// A capacity whose byte size overflows the address space is a different case:
/// it is not a "no block" request but an impossible one, so it aborts rather
/// than returning a `None` a caller would misread as empty and then write
/// through a null pointer.
fn items_layout(cap: usize, esize: usize) -> Option<Layout> {
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

/// Allocates an item block for `cap` elements, or null when it would be empty.
///
/// A failed allocation aborts through [`alloc::handle_alloc_error`] — the same
/// response `Box`/`Vec` give — rather than returning the null `alloc` hands back
/// on failure, which the callers would then write through.
fn alloc_items(cap: usize, esize: usize) -> *mut u8 {
    match items_layout(cap, esize) {
        Some(layout) => {
            // SAFETY: the layout is non-zero-sized, which is what `alloc`
            // requires.
            let items = unsafe { alloc::alloc(layout) };
            if items.is_null() {
                alloc::handle_alloc_error(layout);
            }
            items
        }
        None => std::ptr::null_mut(),
    }
}

/// Allocates an array of `count` elements, with `len == cap == count`.
///
/// A literal's array is full the moment it exists; an empty one gets no item
/// block at all, and the first append is what buys one.
#[unsafe(no_mangle)]
pub extern "C" fn kira_rt_array_new(count: usize, esize: usize) -> KArray {
    Box::into_raw(Box::new(KiraArray {
        len: count,
        cap: count,
        items: alloc_items(count, esize),
    }))
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

/// The address of element `index`, bounds-checked.
///
/// The only place a bounds check lives: every element read and every element
/// write goes through this, so neither can forget it. A negative index and one
/// past the end are **different traps**, because they are different mistakes —
/// the VM draws the same line.
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

/// Makes room for one more element and returns where it goes.
///
/// Capacity doubles (from one, when there is none), so a run of appends copies
/// O(n) elements in total rather than O(n²).
///
/// # Safety
/// `array` must be a live handle from this runtime, and `esize` must be the
/// element size it was built with.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_rt_array_push_slot(array: KArray, esize: usize) -> *mut u8 {
    // SAFETY: the caller guarantees a live handle.
    let header = unsafe { &mut *array };
    if header.len == header.cap {
        let grown = if header.cap == 0 { 1 } else { header.cap * 2 };
        let fresh = alloc_items(grown, esize);
        if !header.items.is_null() {
            // SAFETY: both blocks hold at least `len * esize` bytes and cannot
            // overlap — `fresh` is a new allocation.
            unsafe { std::ptr::copy_nonoverlapping(header.items, fresh, header.len * esize) };
            if let Some(layout) = items_layout(header.cap, esize) {
                // SAFETY: `items` came from `alloc` with exactly this layout.
                unsafe { alloc::dealloc(header.items, layout) };
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

/// Produces an independent copy of an array.
///
/// Flat first: one `memcpy` of the whole item block. For an array of scalars
/// that is the entire job, and `element` is null. Otherwise the block holds the
/// *source's* handles, and the callback replaces each with a copy — which is
/// what stops a write through one array being visible through the other.
///
/// # Safety
/// `array` must be a live handle from this runtime; `esize` must be the element
/// size it was built with; and `element`, when given, must clone exactly one
/// element of that size.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_rt_array_clone(
    array: KArray,
    esize: usize,
    element: Option<ElemClone>,
) -> KArray {
    if array.is_null() {
        return kira_rt_array_new(0, esize);
    }
    // SAFETY: the caller guarantees a live handle.
    let source = unsafe { &*array };
    let copy = kira_rt_array_new(source.len, esize);
    // SAFETY: `copy` is a handle this function just built.
    let target = unsafe { &mut *copy };
    if source.len > 0 && !source.items.is_null() {
        // SAFETY: both blocks hold at least `len * esize` bytes and cannot
        // overlap — `target.items` is a new allocation.
        unsafe {
            std::ptr::copy_nonoverlapping(source.items, target.items, source.len * esize);
        }
    }
    if let Some(clone) = element {
        for at in 0..source.len {
            // SAFETY: both offsets land inside blocks of `len` elements, and
            // the callback is the caller's promise to handle one of `esize`.
            unsafe {
                clone(source.items.add(at * esize), target.items.add(at * esize));
            }
        }
    }
    copy
}

/// Frees an array and whatever its elements own.
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
    // SAFETY: the caller guarantees a live handle it is giving up.
    let header = unsafe { Box::from_raw(array) };
    if !header.items.is_null() {
        if let Some(free) = element {
            for at in 0..header.len {
                // SAFETY: the offset lands inside a block of `len` elements.
                unsafe { free(header.items.add(at * esize)) };
            }
        }
        if let Some(layout) = items_layout(header.cap, esize) {
            // SAFETY: `items` came from `alloc` with exactly this layout.
            unsafe { alloc::dealloc(header.items, layout) };
        }
    }
}

/// Reports an out-of-range array index and exits with a failure code, mirroring
/// the VM's `IndexOutOfBounds` trap: no further output, non-zero exit.
#[unsafe(no_mangle)]
pub extern "C" fn kira_rt_trap_index_out_of_bounds() -> ! {
    eprintln!("kira: runtime trap: array index is out of bounds");
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
                let slot = kira_rt_array_push_slot(array, ESIZE);
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
                *kira_rt_array_push_slot(array, ESIZE).cast::<i64>() = value;
            }
            // The whole design: a holder of the handle still sees the growth.
            assert_eq!(before, array);
            assert_eq!(read(array, 63), 63);
            kira_rt_array_free(array, ESIZE, None);
        }
    }

    #[test]
    fn a_clone_of_a_scalar_array_shares_no_storage() {
        let array = kira_rt_array_new(2, ESIZE);
        // SAFETY: a handle this test just built.
        unsafe {
            *(*array).items.cast::<i64>() = 1;
            *(*array).items.add(ESIZE).cast::<i64>() = 2;

            let copy = kira_rt_array_clone(array, ESIZE, None);
            assert_ne!((*array).items, (*copy).items, "the block is its own");

            *(*copy).items.cast::<i64>() = 99;
            assert_eq!(read(array, 0), 1, "the original is untouched");
            assert_eq!(read(copy, 0), 99);

            kira_rt_array_free(array, ESIZE, None);
            kira_rt_array_free(copy, ESIZE, None);
        }
    }

    #[test]
    fn a_clone_of_an_empty_array_is_an_empty_array() {
        let array = kira_rt_array_new(0, ESIZE);
        // SAFETY: a handle this test just built.
        unsafe {
            let copy = kira_rt_array_clone(array, ESIZE, None);
            assert_eq!(kira_rt_array_len(copy), 0);
            kira_rt_array_free(array, ESIZE, None);
            kira_rt_array_free(copy, ESIZE, None);
        }
    }

    /// The element callback is what makes a clone deep. This stands in for the
    /// leaf the backend emits for a `String` element.
    #[test]
    fn a_clone_runs_the_element_callback_over_every_element() {
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
            let copy = kira_rt_array_clone(array, ESIZE, Some(bump));
            for at in 0..3usize {
                assert_eq!(read(copy, at), at as i64 + 100);
                assert_eq!(read(array, at), at as i64);
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
                *kira_rt_array_push_slot(array, ESIZE).cast::<i64>() = value;
            }
            // Capacity is 8 by now, but only 5 elements are live.
            assert_eq!((*array).cap, 8);
            kira_rt_array_free(array, ESIZE, Some(count));
        }
        assert_eq!(FREED.load(Ordering::Relaxed), 5);
    }

    #[test]
    fn a_null_handle_is_a_free_that_does_nothing() {
        // SAFETY: null is explicitly allowed.
        unsafe {
            kira_rt_array_free(std::ptr::null_mut(), ESIZE, None);
            assert_eq!(kira_rt_array_len(std::ptr::null_mut()), 0);
        }
    }
}
