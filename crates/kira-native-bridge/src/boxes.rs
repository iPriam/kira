//! The storage an exported class instance lives in when it crosses to a
//! consumer as a handle.
//!
//! # Why a handle needs a box at all
//!
//! Inside native Kira code a class instance is an LLVM struct *value*: it lives
//! in registers or on the stack, and it dies when the frame does. A handle
//! outlives the call that made it by definition — a consumer holds a `Button`
//! across many calls — so the value has to be moved somewhere that survives the
//! return. That is this: one allocation per handle, holding exactly the struct's
//! bytes.
//!
//! # Why the runtime owns the allocator and generated code does not
//!
//! Generated code has no allocator. It could call libc's `malloc`, but then a
//! Kira library would allocate from two heaps — the Rust one every `kira_rt_*`
//! helper uses, and libc's — for no benefit and one more thing to get wrong on a
//! platform where they differ. So the box is allocated and freed here, by the
//! same runtime that owns every other Kira allocation.
//!
//! # The contract
//!
//! Symmetric and total: `kira_rt_box_new(size)` returns storage for `size`
//! bytes, and `kira_rt_box_free(ptr, size)` releases it with the *same* size.
//! Rust's allocator requires the layout back, which is why the size travels in
//! both directions rather than being remembered in a header — a header would be
//! a second layout for the backend to agree with, and this needs none.
//!
//! The box holds the struct's bytes and nothing else. Whatever those bytes own —
//! a `KStr` in a field, a `KArray` — is released by the generated drop
//! trampoline *before* the box is freed, exactly as the VM drops a value before
//! releasing its slot.
//!
//! Every symbol is `extern "C"` with a `kira_rt_` prefix and a fixed signature.
//! These names are a wire contract with the backend's lowering and are
//! append-only: never rename one or change a signature in place.

use std::alloc::{self, Layout};

/// The alignment every box is given.
///
/// Eight bytes covers every scalar a Kira struct field can be — `i64`, `f64`,
/// `bool`, and every opaque handle (`KStr`, `KArray`, `KEnum`) is a pointer —
/// and a struct is never more aligned than its most-aligned field. Fixed rather
/// than passed in because a wrong answer here is undefined behavior on a target
/// that cares, and the backend has no reason to know a number this crate can
/// state once.
const BOX_ALIGN: usize = 8;

/// The layout a box of `size` bytes is allocated and freed under.
///
/// Zero is rounded up to one: a class with no fields is a legal Kira class, and
/// a zero-sized allocation is undefined behavior in Rust's allocator. One byte
/// costs nothing and makes the handle a real, distinct address — which is what
/// lets a consumer hold two of them and tell them apart.
fn layout_of(size: usize) -> Option<Layout> {
    Layout::from_size_align(size.max(1), BOX_ALIGN).ok()
}

/// Allocates zeroed storage for one exported class instance.
///
/// Returns null when `size` is larger than this platform can lay out, which the
/// caller treats as the allocation failure it is. Zeroed rather than
/// uninitialized so that a box observed before the value is stored into it holds
/// defined bytes — the native mirror of the VM initializing every slot.
///
/// # Safety
/// The returned pointer must be released exactly once with
/// [`kira_rt_box_free`], passing the same `size`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_rt_box_new(size: usize) -> *mut u8 {
    let Some(layout) = layout_of(size) else {
        return std::ptr::null_mut();
    };
    // SAFETY: `layout` has a non-zero size — `layout_of` rounds zero up to one —
    // which is what `alloc_zeroed` requires.
    unsafe { alloc::alloc_zeroed(layout) }
}

/// Releases a box from [`kira_rt_box_new`].
///
/// A null pointer is ignored, so a drop trampoline reached with a handle that
/// was never made is a no-op rather than a crash. Whatever the box's bytes owned
/// must already have been released: this frees the storage, never its contents.
///
/// # Safety
/// `handle` must be null or a live pointer from [`kira_rt_box_new`], and `size`
/// must be the size it was allocated with.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_rt_box_free(handle: *mut u8, size: usize) {
    if handle.is_null() {
        return;
    }
    let Some(layout) = layout_of(size) else {
        return;
    };
    // SAFETY: the caller promises `handle` came from `kira_rt_box_new` with this
    // `size`, so `layout` is the layout it was allocated under.
    unsafe { alloc::dealloc(handle, layout) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_box_is_zeroed_and_writable() {
        // SAFETY: the box is freed once below with the size it was made with.
        unsafe {
            let handle = kira_rt_box_new(24);
            assert!(!handle.is_null());
            for offset in 0..24 {
                assert_eq!(*handle.add(offset), 0, "byte {offset} was not zeroed");
            }
            handle.cast::<i64>().write(0x1234_5678_9abc_def0);
            assert_eq!(handle.cast::<i64>().read(), 0x1234_5678_9abc_def0);
            kira_rt_box_free(handle, 24);
        }
    }

    #[test]
    fn a_class_with_no_fields_still_gets_a_real_address() {
        // A zero-sized allocation is undefined behavior, and a class with no
        // fields is legal Kira. Two of them must also be distinguishable, which
        // is the whole reason a handle is worth holding.
        // SAFETY: both boxes are freed once, with the size they were made with.
        unsafe {
            let first = kira_rt_box_new(0);
            let second = kira_rt_box_new(0);
            assert!(!first.is_null());
            assert!(!second.is_null());
            assert_ne!(first, second);
            kira_rt_box_free(first, 0);
            kira_rt_box_free(second, 0);
        }
    }

    #[test]
    fn freeing_nothing_is_nothing() {
        // SAFETY: a null handle is the documented no-op.
        unsafe { kira_rt_box_free(std::ptr::null_mut(), 16) };
    }

    #[test]
    fn a_size_no_layout_can_hold_reports_failure_rather_than_aborting() {
        // A library never gets to end its caller's process, so an impossible
        // size is a null return the caller can see.
        // SAFETY: nothing is allocated, so nothing is freed.
        let handle = unsafe { kira_rt_box_new(usize::MAX) };
        assert!(handle.is_null());
    }

    #[test]
    fn many_boxes_made_and_freed_leave_the_allocator_balanced() {
        // The discipline is only worth anything under repetition; a leak of one
        // box per call is invisible once and fatal in a UI.
        for _ in 0..300 {
            // SAFETY: each box is freed once with its own size.
            unsafe {
                let handle = kira_rt_box_new(32);
                assert!(!handle.is_null());
                kira_rt_box_free(handle, 32);
            }
        }
    }
}
