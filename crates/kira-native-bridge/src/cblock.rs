//! Uniquely owned C-storage blocks for generated native code.
//!
//! The native half of the contract [`kira_runtime_abi::c_storage`] states:
//! every pointer Kira materializes for C — a NUL-terminated string member, a
//! C-layout image, an array flattened to C widths — is backed by a block with
//! exactly one owner. Generated code clones it when a struct copy needs bytes
//! of its own, frees it when the owner dies, and hands it to [the retained
//! registry](kira_rt_cblock_keep) when a `retains:` parameter transfers it to
//! C. There is no share count: the ownership checker's move rules are what
//! make the single owner real, so a block never needs one.
//!
//! # Representation
//!
//! A handle is the **payload address** — the exact word a C-layout image
//! carries and a callee reads — with a [`CBlockHeader`] at a fixed offset
//! before it. Two kinds exist, told apart by the header's tag:
//!
//! * **Owned** — the payload is the data itself.
//! * **Alien** — the payload is one stored word Kira did not allocate: a
//!   pointer C handed over, or `nullPointer()`. Wrapping it is what lets a
//!   C-layout pointer slot hold *either* kind behind one statically-typed
//!   field, which a walk can clone and free without knowing which it is.
//!   [`kira_rt_cblock_word`] resolves both back to the word C should read.

use std::alloc::{Layout, alloc, dealloc};
use std::sync::Mutex;

use crate::runtime::KStr;

/// The header behind every block handle, at `handle - size_of::<CBlockHeader>()`.
///
/// `#[repr(C)]` because generated code and this runtime are compiled
/// separately and must agree on the offsets; the layout test beside it is what
/// holds them to it.
#[repr(C)]
struct CBlockHeader {
    /// [`OWNED_TAG`] or [`ALIEN_TAG`].
    tag: u64,
    /// Payload length in bytes.
    len: u64,
    /// Owned child blocks and the payload offsets that point to them.
    children: *mut Vec<CBlockChild>,
    /// Keeps the payload 16-byte aligned on every supported target.
    reserved: u64,
}

/// One child block owned by a parent C-layout image.
#[derive(Clone, Copy)]
struct CBlockChild {
    /// Byte offset of the child pointer in the parent's payload.
    offset: u64,
    /// Width of that pointer word in the target C layout.
    width: u64,
    /// The uniquely owned child handle.
    handle: i64,
}

/// The header tag of a block whose payload is the data itself.
const OWNED_TAG: u64 = 1;

/// The header tag of a block whose payload is one stored foreign word.
const ALIEN_TAG: u64 = 2;

/// Header size; the payload begins this many bytes after the allocation.
const HEADER: usize = size_of::<CBlockHeader>();

/// Payload alignment: enough for any C scalar or descriptor struct.
const ALIGN: usize = 16;

/// Blocks a `retains:` parameter transferred to C.
///
/// C holds their pointers for the rest of the process, so nothing here frees
/// them — the registry exists so the transfer is *counted* rather than
/// invisible, and so an embedder that can prove every callee is done (a hybrid
/// session close) has one place to reclaim from later.
static RETAINED: Mutex<Vec<usize>> = Mutex::new(Vec::new());

/// Allocates a block and copies `bytes` into its payload, returning the handle.
fn new_block(tag: u64, bytes: &[u8]) -> i64 {
    let Some(size) = HEADER.checked_add(bytes.len()) else {
        return 0;
    };
    let Ok(layout) = Layout::from_size_align(size, ALIGN) else {
        return 0;
    };
    // SAFETY: the layout has non-zero size — the header alone is 16 bytes.
    let base = unsafe { alloc(layout) };
    if base.is_null() {
        return 0;
    }
    // SAFETY: `base` addresses `HEADER + bytes.len()` writable bytes; the
    // header is written at offset zero and the payload copy after it does not
    // overlap `bytes`, which is a live borrow of other storage.
    unsafe {
        base.cast::<CBlockHeader>().write(CBlockHeader {
            tag,
            len: bytes.len() as u64,
            children: Box::into_raw(Box::new(Vec::new())),
            reserved: 0,
        });
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), base.add(HEADER), bytes.len());
    }
    crate::accounting::record_alloc();
    (base as usize + HEADER) as i64
}

/// The header behind `handle`, or `None` for null.
///
/// # Safety
/// A non-zero `handle` must have come from this module and not yet been freed.
unsafe fn header_of<'a>(handle: i64) -> Option<&'a CBlockHeader> {
    if handle == 0 {
        return None;
    }
    // SAFETY: the caller guarantees the handle is a live payload address, so
    // the header sits immediately before it.
    Some(unsafe { &*((handle as usize - HEADER) as *const CBlockHeader) })
}

/// Copies a Kira `String` into an owned, NUL-terminated block, returning its
/// handle; consumes the string handle.
///
/// The native mirror of the VM's `CStringNew`. Null for an interior NUL — the
/// bytes C would read are not the bytes Kira holds — and for bytes that are
/// not UTF-8, which no Kira `String` produces.
///
/// # Safety
/// `value` must be null or a live handle from this runtime; it is freed here.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_rt_cblock_text(value: KStr) -> i64 {
    // SAFETY: the caller vouches for the handle; `bytes_of` accepts null.
    let bytes = unsafe { crate::runtime::bytes_of(value) };
    let handle = match std::str::from_utf8(bytes) {
        Ok(text) => match kira_runtime_abi::c_storage::nul_terminated(text) {
            Some(image) => new_block(OWNED_TAG, &image),
            None => 0,
        },
        Err(_) => 0,
    };
    // SAFETY: same handle, given up by the caller.
    unsafe { crate::runtime::drop_handle(value) };
    handle
}

/// Copies `len` bytes into an owned block, returning its handle.
///
/// The native mirror of the VM's `CLayoutAddress` and the flattened-array
/// seam. Null for an empty image, which no C-layout struct has.
///
/// # Safety
/// `src` must address at least `len` initialized bytes for this call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_rt_cblock_bytes(src: *const u8, len: i64) -> i64 {
    if src.is_null() || len <= 0 {
        return 0;
    }
    // SAFETY: the caller guarantees `len` initialized bytes at `src`.
    let bytes = unsafe { std::slice::from_raw_parts(src, len as usize) };
    new_block(OWNED_TAG, bytes)
}

/// Wraps one foreign word in an alien block, returning its handle.
///
/// What a C-layout pointer slot stores when it is filled with a word Kira did
/// not allocate: a pointer read back from C, or `nullPointer()`. The wrap is
/// what keeps every such slot a block, so clone and free walks need no
/// runtime tag on the *slot*.
#[unsafe(no_mangle)]
pub extern "C" fn kira_rt_cblock_alien(word: i64) -> i64 {
    if word == 0 {
        return 0;
    }
    new_block(ALIEN_TAG, &word.to_le_bytes())
}

/// The word C should read for `handle`: an owned block's payload address, an
/// alien block's stored word, zero for null.
///
/// # Safety
/// `handle` must be null or a live handle from this module.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_rt_cblock_word(handle: i64) -> i64 {
    // SAFETY: the caller vouches for the handle.
    match unsafe { header_of(handle) } {
        None => 0,
        Some(header) if header.tag == ALIEN_TAG => {
            // SAFETY: an alien payload is exactly the eight stored bytes.
            unsafe { (handle as usize as *const i64).read_unaligned() }
        }
        Some(_) => handle,
    }
}

/// Clones `handle` into a block of its own, returning the fresh handle.
///
/// A genuinely deep copy — a block has exactly one owner, so a second holder
/// needs bytes of its own at a fresh address. The ownership checker's move
/// rules keep this off every hot path: it runs only where a struct truly
/// copies, never per read.
///
/// # Safety
/// `handle` must be null or a live handle from this module.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_rt_cblock_clone(handle: i64) -> i64 {
    // SAFETY: the caller vouches for the handle.
    let Some(header) = (unsafe { header_of(handle) }) else {
        return 0;
    };
    // SAFETY: a live block's payload is `len` initialized bytes at the handle.
    let bytes =
        unsafe { std::slice::from_raw_parts(handle as usize as *const u8, header.len as usize) };
    let clone = new_block(header.tag, bytes);
    if clone == 0 {
        return 0;
    }
    // SAFETY: every live block owns one live child vector until its free.
    let children = unsafe { &*header.children };
    for child in children.iter().copied() {
        // SAFETY: the parent's ownership tree guarantees every child is live.
        let child_clone = unsafe { kira_rt_cblock_clone(child.handle) };
        if child.handle != 0 && child_clone == 0 {
            // SAFETY: `clone` is still wholly owned by this function.
            unsafe { kira_rt_cblock_free(clone) };
            return 0;
        }
        // SAFETY: the cloned parent has the same payload layout and the cloned
        // child is live and uniquely owned here.
        unsafe { attach(clone, child.offset, child.width, child_clone) };
    }
    clone
}

/// Frees the block behind `handle`. Null is a no-op.
///
/// # Safety
/// `handle` must be null or a live handle from this module, not yet freed and
/// not transferred to the retained registry; it is freed here.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_rt_cblock_free(handle: i64) {
    // SAFETY: the caller vouches for the handle.
    let Some(header) = (unsafe { header_of(handle) }) else {
        return;
    };
    let Some(size) = HEADER.checked_add(header.len as usize) else {
        return;
    };
    let Ok(layout) = Layout::from_size_align(size, ALIGN) else {
        return;
    };
    let children = header.children;
    // SAFETY: this block owns the vector exactly once until this free.
    let children = unsafe { Box::from_raw(children) };
    for child in children.iter().copied() {
        // SAFETY: the ownership tree gives every child exactly one parent.
        unsafe { kira_rt_cblock_free(child.handle) };
    }
    // SAFETY: the block was allocated by `new_block` with exactly this layout,
    // and the caller's free-once contract makes this the only reclaim.
    unsafe { dealloc((handle as usize - HEADER) as *mut u8, layout) };
    crate::accounting::record_free();
}

/// Moves `child` under `parent` and writes its resolved word at `offset`.
///
/// This is how a C-layout image owns every block whose address its bytes
/// contain. `width` is four or eight bytes, matching the target pointer width.
///
/// # Safety
/// `parent` and non-zero `child` must be distinct live handles from this
/// module. `parent` must own a payload with `width` writable bytes at `offset`.
/// Ownership of `child` moves to `parent`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_rt_cblock_attach(parent: i64, offset: i64, width: i64, child: i64) {
    if child == 0 {
        return;
    }
    // SAFETY: the caller supplies the complete ownership and layout contract.
    unsafe { attach(parent, offset as u64, width as u64, child) };
}

/// [`kira_rt_cblock_attach`] without the C ABI conversion.
///
/// # Safety
/// The caller must satisfy [`kira_rt_cblock_attach`]'s contract.
unsafe fn attach(parent: i64, offset: u64, width: u64, child: i64) {
    // SAFETY: the caller guarantees a live parent.
    let Some(header) = (unsafe { header_of(parent) }) else {
        return;
    };
    debug_assert_eq!(header.tag, OWNED_TAG);
    debug_assert!(matches!(width, 4 | 8));
    debug_assert!(
        offset
            .checked_add(width)
            .is_some_and(|end| end <= header.len)
    );
    // SAFETY: the caller guarantees the offset and width are within the live
    // payload and the child is live.
    let word = unsafe { kira_rt_cblock_word(child) }.to_le_bytes();
    // SAFETY: same caller contract; parent payload and child vector remain live
    // for this whole ownership transfer.
    unsafe {
        std::ptr::copy_nonoverlapping(
            word.as_ptr(),
            (parent as usize as *mut u8).add(offset as usize),
            width as usize,
        );
        (*header.children).push(CBlockChild {
            offset,
            width,
            handle: child,
        });
    }
}

/// Transfers the block behind `handle` to the retained registry: alive, and
/// counted, until an embedder that can prove every callee is done reclaims it.
///
/// What a `retains:` foreign parameter does to each block reachable from its
/// argument. Null is a no-op.
///
/// # Safety
/// `handle` must be null or a live handle from this module; ownership moves to
/// the registry and the caller must not free it afterwards.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_rt_cblock_keep(handle: i64) {
    if handle == 0 {
        return;
    }
    match RETAINED.lock() {
        Ok(mut retained) => retained.push(handle as usize),
        Err(poisoned) => poisoned.into_inner().push(handle as usize),
    };
}

/// How many blocks `retains:` parameters have transferred to C so far.
///
/// The observability half of the registry: a program that hands C storage
/// every frame shows up as a climbing count rather than as invisible growth.
#[unsafe(no_mangle)]
pub extern "C" fn kira_rt_cblock_retained_count() -> i64 {
    match RETAINED.lock() {
        Ok(retained) => retained.len() as i64,
        Err(poisoned) => poisoned.into_inner().len() as i64,
    }
}

/// How many C-block allocations are owned by retained roots, including children.
#[unsafe(no_mangle)]
pub extern "C" fn kira_rt_cblock_retained_block_count() -> u64 {
    let roots = match RETAINED.lock() {
        Ok(retained) => retained.clone(),
        Err(poisoned) => poisoned.into_inner().clone(),
    };
    let mut pending = roots;
    let mut count = 0u64;
    while let Some(handle) = pending.pop() {
        count = count.saturating_add(1);
        // SAFETY: every registry root and child remains live until release.
        let Some(header) = (unsafe { header_of(handle as i64) }) else {
            continue;
        };
        // SAFETY: every live block owns one live child vector.
        pending.extend(
            (unsafe { &*header.children })
                .iter()
                .map(|child| child.handle as usize),
        );
    }
    count
}

/// Frees every block transferred by `retains:` parameters.
///
/// An embedder calls this only after it has stopped the native image and proved
/// no foreign callee can read a retained pointer again. Whole-process native
/// programs need no call because process teardown reclaims the address space.
#[unsafe(no_mangle)]
pub extern "C" fn kira_rt_cblock_release_retained() {
    let handles = match RETAINED.lock() {
        Ok(mut retained) => std::mem::take(&mut *retained),
        Err(poisoned) => std::mem::take(&mut *poisoned.into_inner()),
    };
    for handle in handles {
        // SAFETY: the registry took unique ownership in `kira_rt_cblock_keep`.
        unsafe { kira_rt_cblock_free(handle as i64) };
    }
}

/// Moves a native C-block tree into the backend-neutral representation.
///
/// # Safety
/// `handle` must be null or one live uniquely owned handle. It is consumed.
pub(crate) unsafe fn into_native_cblock(handle: i64) -> Option<kira_runtime_abi::NativeCBlock> {
    if handle == 0 {
        return Some(kira_runtime_abi::NativeCBlock::new(Vec::new()));
    }
    // SAFETY: the caller gives unique ownership of this live block.
    let header = unsafe { header_of(handle) }?;
    let len = header.len as usize;
    let children = header.children;
    let size = HEADER.checked_add(len)?;
    let Ok(layout) = Layout::from_size_align(size, ALIGN) else {
        return None;
    };
    // SAFETY: the handle owns `len` initialized payload bytes and one child
    // vector. Both are moved out before the allocation is reclaimed.
    let bytes = unsafe { std::slice::from_raw_parts(handle as usize as *const u8, len).to_vec() };
    // SAFETY: this header owns the vector exactly once.
    let children = unsafe { Box::from_raw(children) };
    // SAFETY: the allocation was created with exactly this layout and its
    // children have been detached from it.
    unsafe { dealloc((handle as usize - HEADER) as *mut u8, layout) };
    crate::accounting::record_free();
    let mut block = kira_runtime_abi::NativeCBlock::new(bytes);
    for child in children.iter().copied() {
        // SAFETY: ownership of every child moved out with the vector.
        let nested = unsafe { into_native_cblock(child.handle) }?;
        let width = match child.width {
            4 => kira_runtime_abi::ForeignPointerWidth::Bits32,
            8 => kira_runtime_abi::ForeignPointerWidth::Bits64,
            _ => return None,
        };
        block
            .attach(
                kira_runtime_abi::CBlockOffset::new(child.offset),
                width,
                nested,
            )
            .ok()?;
    }
    Some(block)
}

/// Materializes one backend-neutral C-block tree in native storage.
pub(crate) fn from_native_cblock(block: kira_runtime_abi::NativeCBlock) -> i64 {
    let (bytes, children) = block.into_parts();
    let root = new_block(OWNED_TAG, &bytes);
    if root == 0 {
        return 0;
    }
    for child in children {
        let offset = child.offset().bytes();
        let width = u64::from(child.width().bytes());
        let nested = from_native_cblock(child.into_block());
        if nested == 0 {
            // SAFETY: `root` is still uniquely owned here.
            unsafe { kira_rt_cblock_free(root) };
            return 0;
        }
        // SAFETY: `NativeCBlock::attach` validated this offset and width.
        unsafe { attach(root, offset, width, nested) };
    }
    root
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Generated code GEPs from a payload back to its header, so the header's
    /// size and field offsets are ABI. This is the layout test that holds
    /// `CBlockHeader` to them.
    #[test]
    fn the_header_layout_is_the_abi() {
        assert_eq!(size_of::<CBlockHeader>(), 32);
        assert_eq!(std::mem::offset_of!(CBlockHeader, tag), 0);
        assert_eq!(std::mem::offset_of!(CBlockHeader, len), 8);
        assert_eq!(std::mem::offset_of!(CBlockHeader, children), 16);
        assert_eq!(std::mem::offset_of!(CBlockHeader, reserved), 24);
        assert_eq!(
            HEADER % ALIGN,
            0,
            "the payload inherits the allocation's alignment"
        );
    }

    #[test]
    fn an_owned_block_resolves_to_its_own_payload() {
        // SAFETY: the handle is live from `new_block` until the free below.
        unsafe {
            let handle = kira_rt_cblock_bytes([1u8, 2, 3, 4].as_ptr(), 4);
            assert_ne!(handle, 0);
            assert_eq!(kira_rt_cblock_word(handle), handle);
            let payload = std::slice::from_raw_parts(handle as usize as *const u8, 4);
            assert_eq!(payload, &[1, 2, 3, 4]);
            kira_rt_cblock_free(handle);
        }
    }

    #[test]
    fn an_alien_block_resolves_to_its_stored_word() {
        // SAFETY: the handle is live from `new_block` until the free below.
        unsafe {
            let handle = kira_rt_cblock_alien(0x1234_5678);
            assert_ne!(handle, 0);
            assert_ne!(kira_rt_cblock_word(handle), handle);
            assert_eq!(kira_rt_cblock_word(handle), 0x1234_5678);
            kira_rt_cblock_free(handle);
        }
    }

    #[test]
    fn a_clone_is_an_independent_block_with_the_same_bytes() {
        // SAFETY: both handles are live from their allocations to their frees.
        unsafe {
            let original = kira_rt_cblock_bytes([9u8, 8, 7].as_ptr(), 3);
            let clone = kira_rt_cblock_clone(original);
            assert_ne!(clone, 0);
            assert_ne!(clone, original);
            kira_rt_cblock_free(original);
            let payload = std::slice::from_raw_parts(clone as usize as *const u8, 3);
            assert_eq!(payload, &[9, 8, 7]);
            kira_rt_cblock_free(clone);
        }
    }

    #[test]
    fn an_image_clone_and_engine_round_trip_rewrite_child_addresses() {
        // SAFETY: every handle is live until ownership moves or its free below.
        unsafe {
            let child = kira_rt_cblock_bytes([4u8, 5, 6].as_ptr(), 3);
            let image = kira_rt_cblock_bytes([0u8; 8].as_ptr(), 8);
            kira_rt_cblock_attach(image, 0, 8, child);
            let clone = kira_rt_cblock_clone(image);
            let image_word = (image as usize as *const i64).read_unaligned();
            let clone_word = (clone as usize as *const i64).read_unaligned();
            assert_eq!(image_word, child);
            assert_ne!(clone_word, image_word);
            let portable = into_native_cblock(image).expect("the live tree moves out");
            let moved = from_native_cblock(portable);
            let moved_word = (moved as usize as *const i64).read_unaligned();
            assert_ne!(moved_word, image_word);
            let payload = std::slice::from_raw_parts(moved_word as usize as *const u8, 3);
            assert_eq!(payload, &[4, 5, 6]);
            kira_rt_cblock_free(clone);
            kira_rt_cblock_free(moved);
        }
    }

    #[test]
    fn null_is_absorbed_everywhere() {
        // SAFETY: null is the documented no-op for every entry point.
        unsafe {
            assert_eq!(kira_rt_cblock_word(0), 0);
            assert_eq!(kira_rt_cblock_clone(0), 0);
            kira_rt_cblock_free(0);
            kira_rt_cblock_keep(0);
        }
    }

    #[test]
    fn keep_transfers_and_counts() {
        let before = kira_rt_cblock_retained_count();
        // SAFETY: the handle is live and ownership moves to the registry.
        unsafe {
            let handle = kira_rt_cblock_bytes([1u8].as_ptr(), 1);
            kira_rt_cblock_keep(handle);
        }
        assert_eq!(kira_rt_cblock_retained_count(), before + 1);
    }
}
