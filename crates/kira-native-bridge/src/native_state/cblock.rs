//! Portable C-block node constructors and readers.

use super::*;

/// Creates a C-block node by copying `len` bytes from `data`.
///
/// The bytes move with the node — see
/// [`kira_runtime_abi::NativeStateValue::CBlock`] — so whichever engine
/// absorbs it materializes a block it owns.
///
/// # Safety
/// `data` must address `len` readable bytes, or be null when `len == 0`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_rt_native_value_cblock(
    data: *const u8,
    len: usize,
    child_count: usize,
) -> KNativeStateValue {
    let bytes = if len == 0 || data.is_null() {
        Vec::new()
    } else {
        // SAFETY: the caller guarantees `len` readable bytes at `data`.
        unsafe { std::slice::from_raw_parts(data, len) }.to_vec()
    };
    Box::into_raw(Box::new(NativeStateNode {
        value: NodeValue::CBlock {
            bytes: bytes.into_boxed_slice(),
            children: std::iter::repeat_with(|| None).take(child_count).collect(),
        },
    }))
}

/// Moves one C-block child into a builder slot.
///
/// # Safety
/// Non-null pointers must name live nodes from this runtime. On success the
/// child is consumed; on failure it remains the caller's.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_rt_native_value_set_cblock_child(
    parent: KNativeStateValue,
    index: usize,
    offset: u64,
    width: u32,
    child: KNativeStateValue,
) -> u32 {
    if parent.is_null() || child.is_null() {
        return NativeStateStatus::MALFORMED_VALUE.0;
    }
    let width = match width {
        4 => ForeignPointerWidth::Bits32,
        8 => ForeignPointerWidth::Bits64,
        _ => return NativeStateStatus::MALFORMED_VALUE.0,
    };
    // SAFETY: the caller vouches both pointers are live.
    let NodeValue::CBlock { children, .. } = (unsafe { &mut (*parent).value }) else {
        return NativeStateStatus::MALFORMED_VALUE.0;
    };
    let Some(slot) = children.get_mut(index) else {
        return NativeStateStatus::MALFORMED_VALUE.0;
    };
    if slot.is_some() {
        return NativeStateStatus::MALFORMED_VALUE.0;
    }
    // SAFETY: the caller vouches `child` is live; inspect before consuming so
    // a wrong-shaped child remains the caller's on failure.
    let child_value = unsafe { &(*child).value };
    if !matches!(
        child_value,
        NodeValue::Ready(NativeStateValue::CBlock(_)) | NodeValue::CBlock { .. }
    ) {
        return NativeStateStatus::MALFORMED_VALUE.0;
    }
    let block = match finish(child) {
        Ok(NativeStateValue::CBlock(block)) => block,
        Ok(_) | Err(_) => return NativeStateStatus::MALFORMED_VALUE.0,
    };
    *slot = Some(CBlockBuilderChild {
        offset: CBlockOffset::new(offset),
        width,
        block,
    });
    NativeStateStatus::OK.0
}

/// Creates a C-block node by consuming one native C-block handle.
///
/// # Safety
/// `handle` must be null or one live uniquely owned C-block handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_rt_native_value_cblock_from_handle(handle: i64) -> KNativeStateValue {
    // SAFETY: ownership of the live handle moves into this node.
    match unsafe { crate::cblock::into_native_cblock(handle) } {
        Some(block) => boxed(NativeStateValue::CBlock(block)),
        None => std::ptr::null_mut(),
    }
}

/// Consumes a C-block node into one native C-block handle.
///
/// # Safety
/// `node` must be null or one live node from this runtime.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_rt_native_value_cblock_to_handle(node: KNativeStateValue) -> i64 {
    match finish(node) {
        Ok(NativeStateValue::CBlock(block)) => crate::cblock::from_native_cblock(block),
        Ok(_) | Err(_) => 0,
    }
}

/// Returns a C-block node's payload length, or zero for another shape.
///
/// # Safety
/// `node` must be null or a live node from this runtime.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_rt_native_value_read_cblock_len(node: KNativeStateValue) -> usize {
    if node.is_null() {
        return 0;
    }
    // SAFETY: the caller vouches the node is live.
    match unsafe { &(*node).value } {
        NodeValue::Ready(NativeStateValue::CBlock(block)) => block.bytes().len(),
        NodeValue::CBlock { bytes, .. } => bytes.len(),
        _ => 0,
    }
}

/// Borrows a C-block node's payload bytes, or null for another shape.
///
/// The pointer is valid until the node is freed or written through.
///
/// # Safety
/// `node` must be null or a live node from this runtime.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_rt_native_value_read_cblock_data(
    node: KNativeStateValue,
) -> *const u8 {
    if node.is_null() {
        return std::ptr::null();
    }
    // SAFETY: the caller vouches the node is live.
    match unsafe { &(*node).value } {
        NodeValue::Ready(NativeStateValue::CBlock(block)) => block.bytes().as_ptr(),
        NodeValue::CBlock { bytes, .. } => bytes.as_ptr(),
        _ => std::ptr::null(),
    }
}

/// Returns one C-block child's embedded byte offset.
///
/// # Safety
/// `node` must be null or a live C-block node from this runtime.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_rt_native_value_cblock_child_offset(
    node: KNativeStateValue,
    index: usize,
) -> u64 {
    if node.is_null() {
        return u64::MAX;
    }
    // SAFETY: the caller vouches the node is live.
    match unsafe { &(*node).value } {
        NodeValue::Ready(NativeStateValue::CBlock(block)) => block
            .children()
            .get(index)
            .map_or(u64::MAX, |child| child.offset().bytes()),
        NodeValue::CBlock { children, .. } => children
            .get(index)
            .and_then(Option::as_ref)
            .map_or(u64::MAX, |child| child.offset.bytes()),
        _ => u64::MAX,
    }
}

/// Returns one C-block child's pointer width in bytes.
///
/// # Safety
/// `node` must be null or a live C-block node from this runtime.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_rt_native_value_cblock_child_width(
    node: KNativeStateValue,
    index: usize,
) -> u32 {
    if node.is_null() {
        return 0;
    }
    // SAFETY: the caller vouches the node is live.
    match unsafe { &(*node).value } {
        NodeValue::Ready(NativeStateValue::CBlock(block)) => block
            .children()
            .get(index)
            .map_or(0, |child| child.width().bytes()),
        NodeValue::CBlock { children, .. } => children
            .get(index)
            .and_then(Option::as_ref)
            .map_or(0, |child| child.width.bytes()),
        _ => 0,
    }
}
