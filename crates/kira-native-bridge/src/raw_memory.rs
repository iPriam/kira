//! Raw memory: allocation and unaligned field access at a byte offset.
//!
//! A native backend that lays out its own C-shaped context — a Metal renderer
//! keeping a device, a queue, and a resource table in one block — needs to
//! allocate bytes and read and write fields inside them. These are the
//! primitives it does that with.
//!
//! # Why every access is unaligned
//!
//! The caller chooses the offsets. A block laid out by a shader backend or a
//! graphics context puts a pointer wherever its own layout says, and nothing
//! here can promise that lands on an 8-byte boundary. Reading it with an
//! aligned load would be undefined behaviour on the day it does not — so every
//! access below goes through `read_unaligned`/`write_unaligned`, which costs
//! nothing on the platforms Kira targets and is correct on all of them.
//!
//! # Allocation
//!
//! Blocks come from the Rust allocator with an 8-byte alignment, which is what
//! makes a pointer field inside one safe to place at any 8-aligned offset. The
//! size is remembered in a header immediately before the pointer handed out, so
//! freeing needs only the pointer — a C caller has no place to keep a layout.

use std::alloc::{Layout, alloc, dealloc};
use std::ffi::c_void;

/// The alignment every block is allocated with.
const BLOCK_ALIGN: usize = 8;

/// The bytes reserved before a block to remember its size.
const HEADER: usize = 8;

/// Allocates `size` zeroed bytes, or returns null when it cannot.
///
/// Zeroed rather than uninitialized: a caller writes the fields it knows and
/// reads the rest expecting nothing, and a context whose unset slots held
/// whatever the allocator last had there would be a bug that reproduces once a
/// month.
#[unsafe(no_mangle)]
pub extern "C" fn kira_dynamic_alloc(size: u64) -> *mut c_void {
    let Ok(size) = usize::try_from(size) else {
        return std::ptr::null_mut();
    };
    let Some(total) = size.checked_add(HEADER) else {
        return std::ptr::null_mut();
    };
    let Ok(layout) = Layout::from_size_align(total, BLOCK_ALIGN) else {
        return std::ptr::null_mut();
    };
    // SAFETY: the layout has a non-zero size — the header alone guarantees it —
    // and an alignment that is a power of two.
    let base = unsafe { alloc(layout) };
    if base.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: `base` addresses `total` writable bytes, and the header plus the
    // body is exactly that.
    unsafe {
        base.cast::<u64>().write_unaligned(size as u64);
        base.add(HEADER).write_bytes(0, size);
        base.add(HEADER).cast::<c_void>()
    }
}

/// Frees a block from [`kira_dynamic_alloc`].
///
/// A null pointer frees nothing, which is what C callers expect of `free`.
///
/// # Safety
/// `ptr` must be null, or a pointer returned by [`kira_dynamic_alloc`] that has
/// not already been freed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_dynamic_free(ptr: *mut c_void) {
    if ptr.is_null() {
        return;
    }
    // SAFETY: the caller guarantees `ptr` came from `kira_dynamic_alloc`, so
    // the header sits immediately before it and holds the body's size.
    unsafe {
        let base = ptr.cast::<u8>().sub(HEADER);
        let size = base.cast::<u64>().read_unaligned() as usize;
        let Some(total) = size.checked_add(HEADER) else {
            return;
        };
        let Ok(layout) = Layout::from_size_align(total, BLOCK_ALIGN) else {
            return;
        };
        dealloc(base, layout);
    }
}

/// The null pointer, for a caller with no other way to write one.
#[unsafe(no_mangle)]
pub extern "C" fn kira_dynamic_null_ptr() -> *mut c_void {
    std::ptr::null_mut()
}

/// Whether `ptr` is null.
#[unsafe(no_mangle)]
pub extern "C" fn kira_dynamic_ptr_is_null(ptr: *const c_void) -> bool {
    ptr.is_null()
}

/// Defines one unaligned reader at a byte offset.
macro_rules! reader {
    ($name:ident, $ty:ty, $doc:literal) => {
        #[doc = $doc]
        ///
        /// # Safety
        /// `ptr` must address a block with at least `offset` plus this value's
        /// size readable bytes.
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn $name(ptr: *const c_void, offset: u64) -> $ty {
            if ptr.is_null() {
                return Default::default();
            }
            let Ok(offset) = usize::try_from(offset) else {
                return Default::default();
            };
            // SAFETY: the caller guarantees the block is readable through this
            // field; the read is unaligned because the caller chose the offset.
            unsafe { ptr.cast::<u8>().add(offset).cast::<$ty>().read_unaligned() }
        }
    };
}

/// Defines one unaligned writer at a byte offset.
macro_rules! writer {
    ($name:ident, $ty:ty, $doc:literal) => {
        #[doc = $doc]
        ///
        /// # Safety
        /// `ptr` must address a block with at least `offset` plus this value's
        /// size writable bytes.
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn $name(ptr: *mut c_void, offset: u64, value: $ty) {
            if ptr.is_null() {
                return;
            }
            let Ok(offset) = usize::try_from(offset) else {
                return;
            };
            // SAFETY: the caller guarantees the block is writable through this
            // field; the write is unaligned because the caller chose the offset.
            unsafe {
                ptr.cast::<u8>()
                    .add(offset)
                    .cast::<$ty>()
                    .write_unaligned(value);
            }
        }
    };
}

reader!(
    kira_dynamic_read_u8_at,
    u8,
    "Reads the byte at `offset` bytes into `ptr`."
);
reader!(
    kira_dynamic_read_u32_at,
    u32,
    "Reads the 32-bit word at `offset` bytes into `ptr`."
);
reader!(
    kira_dynamic_read_u64_at,
    u64,
    "Reads the 64-bit word at `offset` bytes into `ptr`."
);
reader!(
    kira_dynamic_read_i32_at,
    i32,
    "Reads the signed 32-bit word at `offset` bytes into `ptr`."
);
reader!(
    kira_dynamic_read_i64_at,
    i64,
    "Reads the signed 64-bit word at `offset` bytes into `ptr`."
);
reader!(
    kira_dynamic_read_f32_at,
    f32,
    "Reads the 32-bit float at `offset` bytes into `ptr`."
);
reader!(
    kira_dynamic_read_f64_at,
    f64,
    "Reads the 64-bit float at `offset` bytes into `ptr`."
);
reader!(
    kira_dynamic_read_u16_at,
    u16,
    "Reads the 16-bit word at `offset` bytes into `ptr`."
);

writer!(
    kira_dynamic_write_u8_at,
    u8,
    "Writes the byte at `offset` bytes into `ptr`."
);
writer!(
    kira_dynamic_write_u16_at,
    u16,
    "Writes the 16-bit word at `offset` bytes into `ptr`."
);
writer!(
    kira_dynamic_write_u32_at,
    u32,
    "Writes the 32-bit word at `offset` bytes into `ptr`."
);
writer!(
    kira_dynamic_write_u64_at,
    u64,
    "Writes the 64-bit word at `offset` bytes into `ptr`."
);
writer!(
    kira_dynamic_write_i64_at,
    i64,
    "Writes the signed 64-bit word at `offset` bytes into `ptr`."
);
writer!(
    kira_dynamic_write_f32_at,
    f32,
    "Writes the 32-bit float at `offset` bytes into `ptr`."
);
writer!(
    kira_dynamic_write_f64_at,
    f64,
    "Writes the 64-bit float at `offset` bytes into `ptr`."
);

/// Reads the pointer at `offset` bytes into `ptr`.
///
/// # Safety
/// `ptr` must address a block with at least `offset` plus a pointer's size
/// readable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_dynamic_read_ptr_at(ptr: *const c_void, offset: u64) -> *mut c_void {
    if ptr.is_null() {
        return std::ptr::null_mut();
    }
    let Ok(offset) = usize::try_from(offset) else {
        return std::ptr::null_mut();
    };
    // SAFETY: the caller guarantees the block is readable through this field.
    unsafe {
        ptr.cast::<u8>()
            .add(offset)
            .cast::<*mut c_void>()
            .read_unaligned()
    }
}

/// Writes the pointer at `offset` bytes into `ptr`.
///
/// # Safety
/// `ptr` must address a block with at least `offset` plus a pointer's size
/// writable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_dynamic_write_ptr_at(
    ptr: *mut c_void,
    offset: u64,
    value: *mut c_void,
) {
    if ptr.is_null() {
        return;
    }
    let Ok(offset) = usize::try_from(offset) else {
        return;
    };
    // SAFETY: the caller guarantees the block is writable through this field.
    unsafe {
        ptr.cast::<u8>()
            .add(offset)
            .cast::<*mut c_void>()
            .write_unaligned(value);
    }
}

/// Reads the pointer at the start of `ptr`.
///
/// # Safety
/// `ptr` must address at least a pointer's worth of readable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_dynamic_read_ptr(ptr: *const c_void) -> *mut c_void {
    // SAFETY: the caller's guarantee is the offset-0 case of the one above.
    unsafe { kira_dynamic_read_ptr_at(ptr, 0) }
}

/// Writes the pointer at the start of `ptr`.
///
/// # Safety
/// `ptr` must address at least a pointer's worth of writable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_dynamic_write_ptr(ptr: *mut c_void, value: *mut c_void) {
    // SAFETY: the caller's guarantee is the offset-0 case of the one above.
    unsafe { kira_dynamic_write_ptr_at(ptr, 0, value) };
}

/// Reads the 32-bit word at the start of `ptr`.
///
/// # Safety
/// `ptr` must address at least four readable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_dynamic_read_u32(ptr: *const c_void) -> u32 {
    // SAFETY: the caller's guarantee is the offset-0 case of the one above.
    unsafe { kira_dynamic_read_u32_at(ptr, 0) }
}

/// Writes the 32-bit word at the start of `ptr`.
///
/// # Safety
/// `ptr` must address at least four writable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_dynamic_write_u32(ptr: *mut c_void, value: u32) {
    // SAFETY: the caller's guarantee is the offset-0 case of the one above.
    unsafe { kira_dynamic_write_u32_at(ptr, 0, value) };
}

/// Reads the signed 32-bit word at the start of `ptr`.
///
/// # Safety
/// `ptr` must address at least four readable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_dynamic_read_i32(ptr: *const c_void) -> i32 {
    // SAFETY: the caller's guarantee is the offset-0 case of the one above.
    unsafe { kira_dynamic_read_i32_at(ptr, 0) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_block_comes_back_zeroed() {
        let block = kira_dynamic_alloc(64);
        assert!(!block.is_null());
        for offset in 0..64u64 {
            // SAFETY: the block is 64 readable bytes.
            assert_eq!(unsafe { kira_dynamic_read_u8_at(block, offset) }, 0);
        }
        // SAFETY: the block came from `kira_dynamic_alloc` and is freed once.
        unsafe { kira_dynamic_free(block) };
    }

    #[test]
    fn a_field_reads_back_what_was_written_at_its_offset() {
        let block = kira_dynamic_alloc(64);
        // SAFETY: every offset below is inside the 64-byte block.
        unsafe {
            kira_dynamic_write_u64_at(block, 0, 0x0102_0304_0506_0708);
            kira_dynamic_write_u32_at(block, 8, 0xdead_beef);
            kira_dynamic_write_u8_at(block, 12, 0x5a);
            kira_dynamic_write_f32_at(block, 16, 1.5);
            assert_eq!(kira_dynamic_read_u64_at(block, 0), 0x0102_0304_0506_0708);
            assert_eq!(kira_dynamic_read_u32_at(block, 8), 0xdead_beef);
            assert_eq!(kira_dynamic_read_u8_at(block, 12), 0x5a);
            assert_eq!(
                kira_dynamic_read_f32_at(block, 16).to_bits(),
                1.5f32.to_bits()
            );
            kira_dynamic_free(block);
        }
    }

    #[test]
    fn a_pointer_survives_an_offset_that_is_not_pointer_aligned() {
        // The whole reason every access is unaligned: a caller's layout may put
        // a pointer at offset 1, and an aligned load there is undefined.
        let block = kira_dynamic_alloc(32);
        let value = kira_dynamic_alloc(8);
        // SAFETY: offset 1 plus a pointer fits inside the 32-byte block.
        unsafe {
            kira_dynamic_write_ptr_at(block, 1, value);
            assert_eq!(kira_dynamic_read_ptr_at(block, 1), value);
            kira_dynamic_free(value);
            kira_dynamic_free(block);
        }
    }

    #[test]
    fn freeing_null_does_nothing() {
        // SAFETY: null is the one pointer the contract explicitly allows.
        unsafe { kira_dynamic_free(std::ptr::null_mut()) };
    }

    #[test]
    fn reading_through_null_answers_a_zero_rather_than_faulting() {
        // SAFETY: null is handled before any dereference.
        unsafe {
            assert_eq!(kira_dynamic_read_u64_at(std::ptr::null(), 0), 0);
            assert!(kira_dynamic_read_ptr_at(std::ptr::null(), 0).is_null());
        }
    }

    #[test]
    fn the_null_pointer_reports_itself_as_null() {
        assert!(kira_dynamic_ptr_is_null(kira_dynamic_null_ptr()));
        let block = kira_dynamic_alloc(8);
        assert!(!kira_dynamic_ptr_is_null(block));
        // SAFETY: the block came from `kira_dynamic_alloc`.
        unsafe { kira_dynamic_free(block) };
    }

    #[test]
    fn a_zero_sized_block_is_still_a_usable_pointer() {
        let block = kira_dynamic_alloc(0);
        assert!(!block.is_null(), "the header alone makes the block real");
        // SAFETY: the block came from `kira_dynamic_alloc`.
        unsafe { kira_dynamic_free(block) };
    }
}
