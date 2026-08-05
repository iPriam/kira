//! The native array runtime: allocation, bounds checking, growth, and the
//! share-on-read/copy-on-write pair.
//!
//! # ABI
//!
//! A Kira array crosses the boundary as a [`KArray`]: an *opaque owned handle*,
//! one pointer, never an aggregate — the same discipline [`crate::runtime`]'s
//! `KStr` follows, and for the same reason. The header behind it is
//! `#[repr(C)]` because the backend reads two of its fields directly, so the
//! two are compiled separately against one layout.
//!
//! ```text
//!   len     how many elements are live
//!   cap     how many the item block has room for
//!   items   the element storage
//!   shares  how many values hold this header
//! ```
//!
//! # Copy on write
//!
//! Value semantics say a copy is independent; they do not say when the copying
//! happens. [`kira_rt_array_clone`] hands back the same header with one more
//! share, so **a copy allocates nothing at all**, and the first write through a
//! shared header is what builds that writer a header and a block of its own.
//!
//! ```text
//!   xs ─┐
//!       ├─▶ { len cap items shares:2 } ──▶ [ e0 e1 e2 … ]
//!   ys ─┘
//! ```
//!
//! Reading an array used to cost the whole array — every element, and every
//! string, array and enum inside every element, cloned and then dropped again.
//! A UI frame is mostly such reads, and they were 78% of one.
//!
//! # A write is given the slot, not the handle
//!
//! Making a header unique replaces it, so a write has to reach whatever *holds*
//! the handle rather than the handle itself: [`kira_rt_array_slot_mut`] and
//! [`kira_rt_array_push_slot`] take `*mut KArray` — the local, the field, or the
//! outer array's element the handle lives in — and store the fresh header back
//! into it.
//!
//! That costs the caller nothing, because every write already starts from a
//! place: the backend's place walk loads the handle *out of* that slot, and now
//! passes the slot instead. It is also what keeps `xs.append(v)` visible through
//! a `borrow mut` parameter, which is a pointer to the caller's slot — the
//! callee's append replaces the header the caller can see.
//!
//! A block belongs to exactly one header, since the write that splits a header
//! copies the block with it, so nothing but the header is ever counted.
//!
//! The count is a plain `usize`, not an atomic: the runtime is single-threaded,
//! as its string and enum storage already assume.
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
//!   `memcpy` a copy starts with is already the whole job.
//!
//! That split is what keeps the *loop* in Rust, where it is ordinary code, and
//! leaves LLVM emitting only leaves.
//!
//! # Ownership
//!
//! Affine, mirroring the VM's heap: reading an array copies it
//! ([`kira_rt_array_clone`]), and a local leaving scope or being overwritten
//! frees it ([`kira_rt_array_free`]). A well-formed program releases every hold
//! exactly once — the same guarantee the VM proves with its heap accounting —
//! and the elements go with the last of them.
//!
//! Every symbol is `extern "C"` with a `kira_rt_` prefix and a fixed signature.
//! These names are a wire contract with the backend's lowering and are
//! append-only: never rename one or change a signature in place.

mod block;

use std::alloc::Layout;

use crate::pool::SharedPool;
use block::{alloc_items, free_items};

/// The free list array headers are handed out from.
///
/// One header per array a program *builds*, and one more each time a write
/// splits a shared one — a copy takes none at all. See [`crate::pool`], which
/// also states what a `static` free list assumes and why the share count below
/// assumes it already.
static HEADERS: SharedPool = SharedPool::new(Layout::new::<KiraArray>());

/// A Kira array at the native ABI: an opaque owned handle.
pub type KArray = *mut KiraArray;

/// Clones one element: read from `src`, write an independent copy to `dst`.
///
/// `dst` already holds a bitwise copy of `src` when this is called, so an
/// implementation replaces exactly the parts that own storage.
pub type ElemClone = unsafe extern "C" fn(src: *const u8, dst: *mut u8);

/// Frees whatever the element at `at` owns. Does not free the slot itself.
pub type ElemFree = unsafe extern "C" fn(at: *mut u8);

/// Compares two elements of one concrete type; non-zero means equal.
pub use crate::enums::ElemEq;

/// The heap object behind a [`KArray`].
///
/// `#[repr(C)]` because the backend reads `shares` directly — copying and
/// releasing an array are a count away from free, and the *call* was the cost —
/// so this layout is a contract with separately compiled code rather than an
/// implementation detail. `the_header_layout_is_pinned` is what holds it, and
/// [`kira_runtime_abi::ARRAY_HEADER_SHARES_FIELD`] is the index the backend uses.
#[repr(C)]
pub struct KiraArray {
    /// How many elements are live.
    pub len: usize,
    /// How many the item block has room for.
    pub cap: usize,
    /// The element storage; null when `cap` is zero.
    pub items: *mut u8,
    /// How many values hold this header.
    ///
    /// One when it is built, one more per copy, one fewer per release, and the
    /// header, its block and everything the elements own go at zero.
    shares: usize,
}

/// Takes a header from the free list and fills it in.
fn new_header(len: usize, cap: usize, items: *mut u8) -> KArray {
    crate::accounting::record_alloc();
    let header = HEADERS.alloc().cast::<KiraArray>();
    // SAFETY: the pool hands back a block of exactly this layout, and every
    // field is written before anything reads one.
    unsafe {
        header.write(KiraArray {
            len,
            cap,
            items,
            shares: 1,
        });
    }
    header
}

/// Gives the handle in `holder` a header and a block of its own.
///
/// A sole holder is the common case and costs a load and a compare. Otherwise
/// the elements are duplicated — flat first, then `element` over each live one,
/// which is exactly the deep copy [`kira_rt_array_clone`] used to do eagerly —
/// into a fresh header, which is stored back into `holder`. The header left
/// behind loses this holder's share.
///
/// A slot that holds no array at all gets an empty one, so an append into a
/// field nobody has built yet lands somewhere rather than through a null.
///
/// # Safety
/// `holder` must address a live slot holding a null or live handle; `esize`
/// must be the element size that array was built with; and `element`, when
/// given, must clone exactly one element of that size.
unsafe fn make_unique(holder: *mut KArray, esize: usize, element: Option<ElemClone>) {
    // SAFETY: the caller guarantees `holder` addresses a live slot.
    let shared = unsafe { *holder };
    if shared.is_null() {
        // SAFETY: as above; the slot takes ownership of the fresh header.
        unsafe { *holder = new_header(0, 0, std::ptr::null_mut()) };
        return;
    }
    // SAFETY: a non-null handle is a live header.
    let source = unsafe { &mut *shared };
    if source.shares == 1 {
        return;
    }
    let items = alloc_items(source.cap, esize);
    // SAFETY: both blocks hold at least `len * esize` bytes and cannot overlap —
    // `items` is a new allocation — and the callback is the caller's promise to
    // handle one element of `esize`.
    unsafe {
        std::ptr::copy_nonoverlapping(source.items, items, source.len * esize);
        if let Some(clone) = element {
            for at in 0..source.len {
                clone(source.items.add(at * esize), items.add(at * esize));
            }
        }
    }
    source.shares -= 1;
    // SAFETY: the caller guarantees `holder` addresses a live slot, which now
    // holds the only handle to the fresh header.
    unsafe { *holder = new_header(source.len, source.cap, items) };
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
/// `array` must be null or a live handle from this runtime.
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

/// Gives a handle a header of its own, so its elements can be moved out of it.
///
/// The runtime's own way in to what [`kira_rt_array_slot_mut`] does before it
/// hands out a writable element. Taking elements *away* is a write like any
/// other: without this, the values still holding the header would read storage
/// that had been moved out from under them.
///
/// # Safety
/// As [`make_unique`]: `holder` must address a live slot, `esize` must match the
/// array, and `element` must clone one element of that size.
pub(crate) unsafe fn make_array_unique(
    holder: *mut KArray,
    esize: usize,
    element: Option<ElemClone>,
) {
    // SAFETY: the caller's promises are exactly `make_unique`'s.
    unsafe { make_unique(holder, esize, element) };
}

/// The address of element `index` **to read**, bounds-checked.
///
/// One of the two places a bounds check lives — [`kira_rt_array_slot_mut`] is
/// the other — so no element access can forget one. A negative index and one
/// past the end are **different traps**, because they are different mistakes —
/// the VM draws the same line.
///
/// The header may be shared, so nothing may be written through this address; a
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

/// The address of element `index` **to write**, bounds-checked, in an array the
/// slot holds alone.
///
/// Takes the slot rather than the handle because making a header unique
/// replaces it; see this module's header. The bounds check comes first, so a
/// trapping index never copies: the program is ending either way, and a copy
/// made on the way out would be one the trap message has to be read past.
///
/// # Safety
/// `holder` must address a live slot holding a live handle from this runtime;
/// `esize` must be the element size it was built with; and `element`, when
/// given, must clone exactly one element of that size.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_rt_array_slot_mut(
    holder: *mut KArray,
    index: i64,
    esize: usize,
    element: Option<ElemClone>,
) -> *mut u8 {
    if index < 0 {
        kira_rt_trap_index_negative();
    }
    // SAFETY: the caller guarantees `holder` addresses a live slot.
    let array = unsafe { *holder };
    if array.is_null() {
        kira_rt_trap_index_out_of_bounds();
    }
    let at = index as usize;
    // SAFETY: a non-null handle is a live header.
    if at >= unsafe { (*array).len } {
        kira_rt_trap_index_out_of_bounds();
    }
    // SAFETY: the caller's promises are exactly `make_unique`'s, and the slot
    // holds the array this element belongs to once it returns.
    unsafe {
        make_unique(holder, esize, element);
        // `at < len <= cap`, so the offset lands inside the item block.
        (**holder).items.add(at * esize)
    }
}

/// Makes room for one more element in an array the slot holds alone, and
/// returns where the element goes.
///
/// Capacity doubles (from one, when there is none), so a run of appends copies
/// O(n) elements in total rather than O(n²).
///
/// # Safety
/// `holder` must address a live slot holding a null or live handle; `esize`
/// must be the element size it was built with; and `element`, when given, must
/// clone exactly one element of that size.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_rt_array_push_slot(
    holder: *mut KArray,
    esize: usize,
    element: Option<ElemClone>,
) -> *mut u8 {
    // Appending is a write, so the array becomes this slot's own before
    // anything lands in it — growth included, which would otherwise abandon a
    // block the values still holding the header count on.
    // SAFETY: the caller's promises are exactly `make_unique`'s.
    unsafe { make_unique(holder, esize, element) };
    // SAFETY: `make_unique` leaves the slot holding a live header.
    let header = unsafe { &mut **holder };
    if header.len == header.cap {
        let grown = if header.cap == 0 { 1 } else { header.cap * 2 };
        let fresh = alloc_items(grown, esize);
        if !header.items.is_null() {
            // SAFETY: both blocks hold at least `len * esize` bytes and cannot
            // overlap — `fresh` is a new allocation — and the old block is this
            // header's alone, so nothing else is reading it.
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

/// Produces a copy of an array: the same header, held once more.
///
/// Independent is a promise about what a reader can observe, not about where
/// the bytes live. Nothing can be written through either array without
/// [`kira_rt_array_slot_mut`] or [`kira_rt_array_push_slot`] first giving that
/// one a header of its own, so the two are indistinguishable from two deep
/// copies — and a copy allocates nothing.
///
/// The backend emits this inline and does not call it; it stays exported
/// because the name is part of the runtime's wire contract.
///
/// # Safety
/// `array` must be null or a live handle from this runtime.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_rt_array_clone(array: KArray) -> KArray {
    if array.is_null() {
        return array;
    }
    // SAFETY: a non-null handle is a live header. The count cannot wrap: it
    // rises by one per live value holding it.
    unsafe { (*array).shares += 1 };
    array
}

/// Whether two arrays hold equal elements, compared with the element's leaf.
///
/// The loop is here for the same reason the clone and free loops are: walking
/// the item block needs no basic blocks, and what this cannot know is how to
/// compare one element. Two arrays of different lengths are unequal without any
/// element being read.
///
/// Neither array is consumed: a comparison reads and takes nothing.
///
/// # Safety
/// Both handles must be null or live, `esize` must be the element stride both
/// were built with, and `element` must compare values of that element type.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_rt_array_eq(
    a: KArray,
    b: KArray,
    esize: usize,
    element: Option<ElemEq>,
) -> u8 {
    // A null handle is an empty array, which is the answer a zeroed slot holds.
    // SAFETY: each length is read only after its handle is proven non-null,
    // and the caller guarantees a non-null handle is live.
    let (a_len, b_len) = unsafe {
        (
            if a.is_null() { 0 } else { (*a).len },
            if b.is_null() { 0 } else { (*b).len },
        )
    };
    if a_len != b_len {
        return 0;
    }
    if a_len == 0 {
        return 1;
    }
    let Some(element) = element else {
        // No leaf means nothing can read these bytes as the type that wrote
        // them; saying "not equal" is the conservative direction.
        return 0;
    };
    // SAFETY: both are non-null with equal, non-zero lengths, so both item
    // blocks hold `a_len` elements of `esize` bytes.
    unsafe {
        let (one, other) = ((*a).items, (*b).items);
        for index in 0..a_len {
            let offset = index * esize;
            if element(one.add(offset), other.add(offset)) == 0 {
                return 0;
            }
        }
    }
    1
}

/// Releases one hold on an array, freeing the header, its block and whatever
/// its elements own once no value holds it.
///
/// `element` is null when the elements own nothing, and then only the block and
/// the header go.
///
/// # Safety
/// `array` must be null or a live handle from this runtime, released once per
/// copy of it that was made; `esize` must be the element size it was built
/// with; and `element`, when given, must free exactly one element of that size.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_rt_array_free(
    array: KArray,
    esize: usize,
    element: Option<ElemFree>,
) {
    if array.is_null() {
        return;
    }
    // SAFETY: a non-null handle is a live header.
    let header = unsafe { &mut *array };
    if header.shares > 1 {
        // Another value still reads these elements, so nothing they own is
        // released here — only this hold on them.
        header.shares -= 1;
        return;
    }
    let (items, len, cap) = (header.items, header.len, header.cap);
    crate::accounting::record_free();
    // SAFETY: this was the last hold, so nothing reads the header again.
    unsafe { HEADERS.free(array.cast::<u8>()) };
    if items.is_null() {
        return;
    }
    if let Some(free) = element {
        for at in 0..len {
            // SAFETY: the offset lands inside a block of `len` elements.
            unsafe { free(items.add(at * esize)) };
        }
    }
    // SAFETY: the block belonged to this header alone.
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
        let mut slot = kira_rt_array_new(0, ESIZE);
        // SAFETY: a slot holding a handle this test just built.
        unsafe {
            for value in 0..10i64 {
                let at = kira_rt_array_push_slot(&raw mut slot, ESIZE, None);
                *at.cast::<i64>() = value;
            }
            assert_eq!(kira_rt_array_len(slot), 10);
            for value in 0..10i64 {
                assert_eq!(read(slot, value as usize), value, "growth kept element");
            }
            // Doubling from one: 1, 2, 4, 8, 16 — never exactly the length.
            assert_eq!((*slot).cap, 16);
            kira_rt_array_free(slot, ESIZE, None);
        }
    }

    /// Growth moves the *items*, and the header a sole holder appends through
    /// stays where it was — which is what a `borrow mut` parameter, a pointer
    /// to the caller's slot, depends on.
    #[test]
    fn the_header_address_survives_growth() {
        let mut slot = kira_rt_array_new(0, ESIZE);
        // SAFETY: a slot holding a handle this test just built.
        unsafe {
            let before = slot;
            for value in 0..64i64 {
                *kira_rt_array_push_slot(&raw mut slot, ESIZE, None).cast::<i64>() = value;
            }
            assert_eq!(before, slot, "nobody else held it, so nothing split");
            assert_eq!(read(slot, 63), 63);
            kira_rt_array_free(slot, ESIZE, None);
        }
    }

    #[test]
    fn a_copy_shares_the_header_until_one_of_them_is_written() {
        let array = kira_rt_array_new(2, ESIZE);
        // SAFETY: a handle this test just built.
        unsafe {
            *(*array).items.cast::<i64>() = 1;
            *(*array).items.add(ESIZE).cast::<i64>() = 2;

            let mut copy = kira_rt_array_clone(array);
            assert_eq!(array, copy, "reading allocates nothing at all");
            assert_eq!((*array).shares, 2);

            // The write is what buys the copy an array of its own, and the
            // original is what it was — which is the whole of value semantics.
            *kira_rt_array_slot_mut(&raw mut copy, 0, ESIZE, None).cast::<i64>() = 99;
            assert_ne!(array, copy, "the slot holds its own header now");
            assert_eq!((*array).shares, 1, "the share came back");
            assert_eq!(read(array, 0), 1, "the original is untouched");
            assert_eq!(read(copy, 0), 99);
            assert_eq!(read(copy, 1), 2, "the rest of the elements came along");

            kira_rt_array_free(array, ESIZE, None);
            kira_rt_array_free(copy, ESIZE, None);
        }
    }

    /// The mirror case: the copy is the one left holding the header, so the
    /// write through the original has to reach the original's own.
    #[test]
    fn writing_the_original_leaves_a_copy_alone() {
        let mut array = kira_rt_array_new(1, ESIZE);
        // SAFETY: a handle this test just built.
        unsafe {
            *(*array).items.cast::<i64>() = 7;
            let copy = kira_rt_array_clone(array);
            *kira_rt_array_slot_mut(&raw mut array, 0, ESIZE, None).cast::<i64>() = 8;
            assert_eq!(read(array, 0), 8);
            assert_eq!(read(copy, 0), 7);
            kira_rt_array_free(array, ESIZE, None);
            kira_rt_array_free(copy, ESIZE, None);
        }
    }

    /// Appending is a write like any other, and it is the one that would
    /// otherwise lengthen an array somebody else is reading.
    #[test]
    fn appending_to_a_copy_leaves_the_original_at_its_length() {
        let array = kira_rt_array_new(2, ESIZE);
        // SAFETY: a handle this test just built.
        unsafe {
            let mut copy = kira_rt_array_clone(array);
            *kira_rt_array_push_slot(&raw mut copy, ESIZE, None).cast::<i64>() = 5;
            assert_eq!(kira_rt_array_len(copy), 3);
            assert_eq!(
                kira_rt_array_len(array),
                2,
                "the original is its own length"
            );
            assert_eq!((*array).shares, 1);
            kira_rt_array_free(array, ESIZE, None);
            kira_rt_array_free(copy, ESIZE, None);
        }
    }

    /// A slot that never held an array is a slot an append can still reach: a
    /// zeroed struct's array field is the null handle until something writes.
    #[test]
    fn appending_into_an_empty_slot_builds_the_array() {
        let mut slot: KArray = std::ptr::null_mut();
        // SAFETY: a null handle is explicitly allowed here.
        unsafe {
            *kira_rt_array_push_slot(&raw mut slot, ESIZE, None).cast::<i64>() = 4;
            assert_eq!(kira_rt_array_len(slot), 1);
            assert_eq!(read(slot, 0), 4);
            kira_rt_array_free(slot, ESIZE, None);
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
    fn splitting_a_header_runs_the_element_callback_over_every_element() {
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
            let mut copy = kira_rt_array_clone(array);
            // Writing element 2 splits the array, so elements 0 and 1 are
            // cloned by the callback rather than left aliasing the original.
            *kira_rt_array_slot_mut(&raw mut copy, 2, ESIZE, Some(bump)).cast::<i64>() = 42;
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
        let mut slot = kira_rt_array_new(0, ESIZE);
        // SAFETY: a slot holding a handle this test just built.
        unsafe {
            for value in 0..5i64 {
                *kira_rt_array_push_slot(&raw mut slot, ESIZE, None).cast::<i64>() = value;
            }
            // Capacity is 8 by now, but only 5 elements are live.
            assert_eq!((*slot).cap, 8);
            kira_rt_array_free(slot, ESIZE, Some(count));
        }
        assert_eq!(FREED.load(Ordering::Relaxed), 5);
    }

    /// What the elements own is released once, by whichever value is last —
    /// never once per holder, which is the double free sharing invites.
    #[test]
    fn a_shared_array_frees_its_elements_once_and_only_at_the_end() {
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
            assert!(
                kira_rt_array_clone(std::ptr::null_mut()).is_null(),
                "a copy of nothing is nothing"
            );
        }
    }

    /// The backend reads `shares` out of this header, so its shape is a
    /// contract with code compiled separately from this crate.
    #[test]
    fn the_header_layout_is_pinned() {
        assert_eq!(size_of::<KiraArray>(), 4 * size_of::<usize>());
        assert_eq!(align_of::<KiraArray>(), align_of::<usize>());
        let header = KiraArray {
            len: 0,
            cap: 0,
            items: std::ptr::null_mut(),
            shares: 1,
        };
        let base = std::ptr::from_ref(&header).cast::<u8>();
        let word = size_of::<usize>() as isize;
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
                word
            );
            assert_eq!(
                std::ptr::from_ref(&header.items)
                    .cast::<u8>()
                    .offset_from(base),
                2 * word
            );
            assert_eq!(
                std::ptr::from_ref(&header.shares)
                    .cast::<u8>()
                    .offset_from(base),
                isize::try_from(kira_runtime_abi::ARRAY_HEADER_SHARES_FIELD)
                    .expect("a small index")
                    * word
            );
        }
    }
}
