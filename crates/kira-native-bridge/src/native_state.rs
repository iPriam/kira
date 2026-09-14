//! Opaque process-lifetime native callback-state storage and value nodes.

use std::sync::{Mutex, OnceLock};

use kira_runtime_abi::{
    CBlockOffset, ForeignPointerWidth, NativeCBlock, NativeCell, NativeStateOwner,
    NativeStatePathStep, NativeStateStatus, NativeStateStore, NativeStateToken, NativeStateValue,
    NativeStateValueTag,
};

use crate::array::{
    ElemClone, KArray, kira_rt_array_free, kira_rt_array_len, kira_rt_array_new,
    kira_rt_array_slot, make_array_unique,
};
use crate::runtime::{KStr, kira_rt_str_data, kira_rt_str_free, kira_rt_str_len, kira_rt_str_new};

mod cblock;
mod cell;

pub use cblock::{
    kira_rt_native_value_cblock, kira_rt_native_value_cblock_child_offset,
    kira_rt_native_value_cblock_child_width, kira_rt_native_value_cblock_from_handle,
    kira_rt_native_value_cblock_to_handle, kira_rt_native_value_read_cblock_data,
    kira_rt_native_value_read_cblock_len, kira_rt_native_value_set_cblock_child,
};
pub use cell::{kira_rt_native_value_cell, kira_rt_native_value_read_cell};

/// Encodes one owned array element from its slot.
pub type NativeStateEncodeElement = unsafe extern "C" fn(*mut u8) -> KNativeStateValue;
/// Decodes one ready value node into a fresh array element slot.
pub type NativeStateDecodeElement = unsafe extern "C" fn(KNativeStateValue, *mut u8);

/// An opaque heap node used while generated code encodes or decodes state.
pub type KNativeStateValue = *mut NativeStateNode;

#[derive(Debug)]
pub struct NativeStateNode {
    value: NodeValue,
}

#[derive(Debug)]
enum NodeValue {
    Ready(NativeStateValue),
    Aggregate {
        tag: NativeStateValueTag,
        enum_tag: u32,
        children: Vec<Option<NativeStateValue>>,
    },
    CBlock {
        bytes: Box<[u8]>,
        children: Vec<Option<CBlockBuilderChild>>,
    },
}

#[derive(Debug)]
struct CBlockBuilderChild {
    offset: CBlockOffset,
    width: ForeignPointerWidth,
    block: NativeCBlock,
}

fn store() -> &'static Mutex<NativeStateStore> {
    static STORE: OnceLock<Mutex<NativeStateStore>> = OnceLock::new();
    STORE.get_or_init(|| Mutex::new(NativeStateStore::new()))
}

fn boxed(value: NativeStateValue) -> KNativeStateValue {
    Box::into_raw(Box::new(NativeStateNode {
        value: NodeValue::Ready(value),
    }))
}

fn status(error: kira_runtime_abi::NativeStateError) -> u32 {
    NativeStateStatus::from(error).0
}

unsafe fn decode_path(
    kinds: *const u8,
    values: *const u64,
    count: usize,
) -> Result<Vec<NativeStatePathStep>, NativeStateStatus> {
    if count == 0 {
        return Ok(Vec::new());
    }
    if kinds.is_null() || values.is_null() {
        return Err(NativeStateStatus::MALFORMED_VALUE);
    }
    // SAFETY: the caller promises both arrays are readable for `count` entries.
    let kinds = unsafe { std::slice::from_raw_parts(kinds, count) };
    // SAFETY: same contract as `kinds`, with one `u64` per step.
    let values = unsafe { std::slice::from_raw_parts(values, count) };
    kinds
        .iter()
        .copied()
        .zip(values.iter().copied())
        .map(|(kind, value)| {
            NativeStatePathStep::from_wire(kind, value)
                .map_err(|_| NativeStateStatus::MALFORMED_VALUE)
        })
        .collect()
}

fn finish(node: KNativeStateValue) -> Result<NativeStateValue, NativeStateStatus> {
    if node.is_null() {
        return Err(NativeStateStatus::MALFORMED_VALUE);
    }
    // SAFETY: the caller gives ownership of one live node.
    let node = unsafe { Box::from_raw(node) };
    match node.value {
        NodeValue::Ready(value) => Ok(value),
        NodeValue::Aggregate {
            tag,
            enum_tag,
            children,
        } => {
            let values: Option<Vec<_>> = children.into_iter().collect();
            let Some(mut values) = values else {
                return Err(NativeStateStatus::MALFORMED_VALUE);
            };
            Ok(match tag {
                NativeStateValueTag::STRUCT => NativeStateValue::struct_of(values),
                NativeStateValueTag::DROP_STRUCT => {
                    NativeStateValue::dropping_struct_of(values, enum_tag)
                }
                NativeStateValueTag::ARRAY => NativeStateValue::array_of(values),
                NativeStateValueTag::ENUM => {
                    if values.len() > 1 {
                        return Err(NativeStateStatus::MALFORMED_VALUE);
                    }
                    NativeStateValue::enum_of(enum_tag, values.pop())
                }
                _ => return Err(NativeStateStatus::MALFORMED_VALUE),
            })
        }
        NodeValue::CBlock { bytes, children } => {
            let children: Option<Vec<_>> = children.into_iter().collect();
            let Some(children) = children else {
                return Err(NativeStateStatus::MALFORMED_VALUE);
            };
            let mut block = NativeCBlock::new(bytes.into_vec());
            for child in children {
                if block
                    .attach(child.offset, child.width, child.block)
                    .is_err()
                {
                    return Err(NativeStateStatus::MALFORMED_VALUE);
                }
            }
            Ok(NativeStateValue::CBlock(block))
        }
    }
}

/// Creates an integer state-value node.
#[unsafe(no_mangle)]
pub extern "C" fn kira_rt_native_value_int(value: i64) -> KNativeStateValue {
    boxed(NativeStateValue::Int(value))
}

/// Creates a `Number` state-value node from its two-word inline value.
#[unsafe(no_mangle)]
pub extern "C" fn kira_rt_native_value_number(value: crate::number::KNumber) -> KNativeStateValue {
    boxed(NativeStateValue::Number(crate::number::unpack(value)))
}

/// Reads a `Number` node back to its two-word value, zero for another shape.
///
/// # Safety
/// `node` must be null or a live node from this runtime.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_rt_native_value_read_number(
    node: KNativeStateValue,
) -> crate::number::KNumber {
    if node.is_null() {
        return crate::number::pack(kira_runtime_abi::Decimal::zero());
    }
    // SAFETY: the caller vouches the node is live.
    match unsafe { &(*node).value } {
        NodeValue::Ready(NativeStateValue::Number(value)) => crate::number::pack(*value),
        _ => crate::number::pack(kira_runtime_abi::Decimal::zero()),
    }
}

/// Creates an erased-value node from its stable type identity and payload.
///
/// The child is consumed into the node, so the caller must not release it
/// after this returns. A dynamic `Any` keeps its identity beside the payload;
/// the receiving backend uses that identity to rebuild the same enum box.
///
/// # Safety
///
/// `payload` is null or one live node allocated by this runtime. A non-null
/// node is consumed exactly once.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_rt_native_value_any(
    type_id: u64,
    payload: KNativeStateValue,
) -> KNativeStateValue {
    let payload = match finish(payload) {
        Ok(payload) => payload,
        Err(_) => return std::ptr::null_mut(),
    };
    boxed(NativeStateValue::any_of(type_id, payload))
}

/// Creates an opaque raw-pointer-word state-value node.
#[unsafe(no_mangle)]
pub extern "C" fn kira_rt_native_value_raw_ptr(value: u64) -> KNativeStateValue {
    boxed(NativeStateValue::RawPtr(value))
}

/// Creates a nested callback-state node by taking ownership of one state reference.
#[unsafe(no_mangle)]
pub extern "C" fn kira_rt_native_value_native_state(value: u64) -> KNativeStateValue {
    let token = NativeStateToken::from_word(value);
    boxed(NativeStateValue::NativeState(NativeStateOwner::new(
        token,
        |token| {
            let status = kira_rt_native_state_retain(token.as_word());
            if status != NativeStateStatus::OK.0 {
                kira_rt_trap_native_state(status);
            }
        },
        |token| {
            let status = kira_rt_native_state_release(token.as_word());
            if status != NativeStateStatus::OK.0 {
                kira_rt_trap_native_state(status);
            }
        },
    )))
}

/// Creates a floating-point state-value node.
#[unsafe(no_mangle)]
pub extern "C" fn kira_rt_native_value_float(value: f64) -> KNativeStateValue {
    boxed(NativeStateValue::Float(value))
}

/// Creates a boolean state-value node from a C byte.
#[unsafe(no_mangle)]
pub extern "C" fn kira_rt_native_value_bool(value: u8) -> KNativeStateValue {
    boxed(NativeStateValue::Bool(value != 0))
}

/// Creates a string node, consuming the Kira string handle.
///
/// # Safety
/// `value` must be null or one live Kira string handle owned by the caller.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_rt_native_value_string(value: KStr) -> KNativeStateValue {
    // SAFETY: the caller vouches the handle is live; the accessors accept null.
    let len = unsafe { kira_rt_str_len(value) };
    // SAFETY: same live-handle guarantee.
    let data = unsafe { kira_rt_str_data(value) };
    let text = if len == 0 {
        String::new()
    } else {
        // SAFETY: the accessor returns `len` readable bytes until the handle is freed.
        let bytes = unsafe { std::slice::from_raw_parts(data, len) };
        String::from_utf8_lossy(bytes).into_owned()
    };
    // SAFETY: ownership moved into this function and is released exactly once.
    unsafe { kira_rt_str_free(value) };
    boxed(NativeStateValue::String(text))
}

/// Creates a struct or array aggregate builder with `count` child slots.
#[unsafe(no_mangle)]
pub extern "C" fn kira_rt_native_value_aggregate(
    tag: u32,
    enum_tag: u32,
    count: usize,
) -> KNativeStateValue {
    Box::into_raw(Box::new(NativeStateNode {
        value: NodeValue::Aggregate {
            tag: NativeStateValueTag(tag),
            enum_tag,
            children: vec![None; count],
        },
    }))
}

/// Encodes an owned Kira array into a generic array node.
///
/// `encode` moves each element out of the block, which is a write: `clone` is
/// what the runtime makes the block this handle's own with first, on the same
/// terms as any other write. See `crate::array`.
///
/// # Safety
/// `array` must be a live owned array with element size `esize`; `clone`, when
/// given, must clone exactly one element of that size; and `encode` must
/// consume one element from each slot and return one live node.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_rt_native_value_array_from(
    array: KArray,
    esize: usize,
    clone: Option<ElemClone>,
    encode: NativeStateEncodeElement,
) -> KNativeStateValue {
    // The elements move out of the array below, which is a write: the values
    // still holding this header would otherwise be left reading what was
    // taken. The handle is this function's own, so its own slot is the holder.
    let mut array = array;
    // SAFETY: `array` is a live local slot and the callback matches.
    unsafe { make_array_unique(&raw mut array, esize, clone) };
    // SAFETY: the caller vouches the array is live.
    let len = unsafe { kira_rt_array_len(array) };
    let count = usize::try_from(len).unwrap_or(0);
    let node = kira_rt_native_value_aggregate(NativeStateValueTag::ARRAY.0, 0, count);
    for index in 0..count {
        // The plain read slot: the block is this handle's own by now, so there
        // is nothing left for a mutable slot to make unique.
        // SAFETY: `index < count == len` and `esize` matches the array.
        let slot = unsafe { kira_rt_array_slot(array, index as i64, esize) };
        // SAFETY: the callback contract matches this live element slot.
        let child = unsafe { encode(slot) };
        // SAFETY: both nodes are live and this slot is written once.
        let status = unsafe { kira_rt_native_value_set_child(node, index, child) };
        if status != NativeStateStatus::OK.0 {
            // SAFETY: `node` is still live and uniquely owned here.
            unsafe { kira_rt_native_value_free(node) };
            return std::ptr::null_mut();
        }
    }
    // Every element's owned contents moved into its node, so only the array block
    // remains to free; passing no element destructor avoids freeing moved handles.
    // SAFETY: the caller gave ownership of this live array and matching size.
    unsafe { kira_rt_array_free(array, esize, None) };
    node
}

/// Decodes a generic array node into a fresh owned Kira array.
///
/// # Safety
/// `node` must be a live array node and `decode` must initialize one element of
/// size `esize` from each child node it consumes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_rt_native_value_array_to(
    node: KNativeStateValue,
    esize: usize,
    decode: NativeStateDecodeElement,
) -> KArray {
    // SAFETY: the caller vouches the node is live.
    let count = unsafe { kira_rt_native_value_len(node) };
    let array = kira_rt_array_new(count, esize);
    for index in 0..count {
        // SAFETY: `node` is live and the index is in range.
        let child = unsafe { kira_rt_native_value_child(node, index) };
        // A block nobody else has seen yet, so the plain read slot is a write
        // slot here: there is no other handle for a copy to protect.
        // SAFETY: the fresh array has exactly `count` slots.
        let slot = unsafe { kira_rt_array_slot(array, index as i64, esize) };
        // SAFETY: the callback consumes `child` and initializes this fresh slot.
        unsafe { decode(child, slot) };
    }
    // SAFETY: the caller gave ownership of the live node.
    unsafe { kira_rt_native_value_free(node) };
    array
}

/// Moves `child` into aggregate slot `index`.
///
/// Returns `MALFORMED_VALUE` for null, non-aggregate, duplicate, or out-of-range
/// input and never dereferences an invalid child after reporting it.
///
/// # Safety
/// Non-null pointers must name live nodes from this runtime. On success ownership
/// of `child` moves into `aggregate`; on failure it remains the caller's.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_rt_native_value_set_child(
    aggregate: KNativeStateValue,
    index: usize,
    child: KNativeStateValue,
) -> u32 {
    if aggregate.is_null() || child.is_null() {
        return NativeStateStatus::MALFORMED_VALUE.0;
    }
    // SAFETY: the caller vouches both pointers are live.
    let aggregate = unsafe { &mut *aggregate };
    let NodeValue::Aggregate { children, .. } = &mut aggregate.value else {
        return NativeStateStatus::MALFORMED_VALUE.0;
    };
    let Some(slot) = children.get_mut(index) else {
        return NativeStateStatus::MALFORMED_VALUE.0;
    };
    if slot.is_some() {
        return NativeStateStatus::MALFORMED_VALUE.0;
    }
    let child = match finish(child) {
        Ok(value) => value,
        Err(status) => return status.0,
    };
    *slot = Some(child);
    NativeStateStatus::OK.0
}

/// Returns a ready node's open value tag, or zero for malformed input.
///
/// # Safety
/// `node` must be null or a live node from this runtime.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_rt_native_value_tag(node: KNativeStateValue) -> u32 {
    if node.is_null() {
        return 0;
    }
    // SAFETY: the caller vouches the node is live.
    match unsafe { &(*node).value } {
        NodeValue::Ready(value) => value_tag(value).0,
        NodeValue::Aggregate { tag, .. } => tag.0,
        NodeValue::CBlock { .. } => NativeStateValueTag::C_BLOCK.0,
    }
}

/// Reads an integer node, returning zero for another shape.
///
/// # Safety
/// `node` must be null or a live node from this runtime.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_rt_native_value_read_int(node: KNativeStateValue) -> i64 {
    if node.is_null() {
        return 0;
    }
    // SAFETY: the caller vouches the node is live.
    match unsafe { &(*node).value } {
        NodeValue::Ready(NativeStateValue::Int(value)) => *value,
        _ => 0,
    }
}

/// Reads the erased type identity from an `Any` node.
///
/// # Safety
///
/// `node` must be null or a live node from this runtime.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_rt_native_value_read_any_type(node: KNativeStateValue) -> u64 {
    if node.is_null() {
        return 0;
    }
    // SAFETY: the caller vouches the node is live.
    match unsafe { &(*node).value } {
        NodeValue::Ready(NativeStateValue::Any { type_id, .. }) => *type_id,
        _ => 0,
    }
}

/// Reads an opaque raw-pointer-word node, returning zero for another shape.
///
/// # Safety
/// `node` must be null or a live node from this runtime.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_rt_native_value_read_raw_ptr(node: KNativeStateValue) -> u64 {
    if node.is_null() {
        return 0;
    }
    // SAFETY: the caller vouches the node is live.
    match unsafe { &(*node).value } {
        NodeValue::Ready(NativeStateValue::RawPtr(value)) => *value,
        _ => 0,
    }
}

/// Retains and reads one nested callback-state owner, returning zero for another shape.
///
/// # Safety
/// `node` must be null or a live node from this runtime.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_rt_native_value_read_native_state(node: KNativeStateValue) -> u64 {
    if node.is_null() {
        return 0;
    }
    // SAFETY: the caller vouches the node is live.
    match unsafe { &(*node).value } {
        NodeValue::Ready(NativeStateValue::NativeState(owner)) => owner.duplicate_token().as_word(),
        _ => 0,
    }
}

/// Reads a floating-point node, returning zero for another shape.
///
/// # Safety
/// `node` must be null or a live node from this runtime.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_rt_native_value_read_float(node: KNativeStateValue) -> f64 {
    if node.is_null() {
        return 0.0;
    }
    // SAFETY: the caller vouches the node is live.
    match unsafe { &(*node).value } {
        NodeValue::Ready(NativeStateValue::Float(value)) => *value,
        _ => 0.0,
    }
}

/// Reads a boolean node, returning zero for another shape.
///
/// # Safety
/// `node` must be null or a live node from this runtime.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_rt_native_value_read_bool(node: KNativeStateValue) -> u8 {
    if node.is_null() {
        return 0;
    }
    // SAFETY: the caller vouches the node is live.
    match unsafe { &(*node).value } {
        NodeValue::Ready(NativeStateValue::Bool(value)) => u8::from(*value),
        _ => 0,
    }
}

/// Clones a string node into a fresh Kira string handle.
///
/// # Safety
/// `node` must be null or a live node from this runtime.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_rt_native_value_read_string(node: KNativeStateValue) -> KStr {
    if node.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: the caller vouches the node is live.
    let NodeValue::Ready(NativeStateValue::String(value)) = (unsafe { &(*node).value }) else {
        return std::ptr::null_mut();
    };
    // SAFETY: the string slice covers exactly its readable bytes.
    unsafe { kira_rt_str_new(value.as_ptr(), value.len()) }
}

/// Returns an aggregate's child count, or zero for another shape.
///
/// # Safety
/// `node` must be null or a live node from this runtime.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_rt_native_value_len(node: KNativeStateValue) -> usize {
    if node.is_null() {
        return 0;
    }
    // SAFETY: the caller vouches the node is live.
    match unsafe { &(*node).value } {
        NodeValue::Ready(NativeStateValue::Struct(values))
        | NodeValue::Ready(NativeStateValue::DropStruct { fields: values, .. })
        | NodeValue::Ready(NativeStateValue::Array(values)) => values.len(),
        NodeValue::Ready(NativeStateValue::Enum { payload, .. }) => usize::from(payload.is_some()),
        NodeValue::Ready(NativeStateValue::Any { .. }) => 1,
        NodeValue::Aggregate { children, .. } => children.len(),
        NodeValue::Ready(NativeStateValue::CBlock(block)) => block.children().len(),
        NodeValue::CBlock { children, .. } => children.len(),
        _ => 0,
    }
}

/// Reads a dropping struct node's user `Drop` glue id, or zero for another shape.
///
/// # Safety
/// `node` must be null or a live node from this runtime.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_rt_native_value_read_drop_glue(node: KNativeStateValue) -> u32 {
    if node.is_null() {
        return 0;
    }
    // SAFETY: the caller vouches the node is live.
    match unsafe { &(*node).value } {
        NodeValue::Ready(NativeStateValue::DropStruct { glue, .. }) => *glue,
        NodeValue::Aggregate {
            tag: NativeStateValueTag::DROP_STRUCT,
            enum_tag,
            ..
        } => *enum_tag,
        _ => 0,
    }
}

/// Returns an enum node's tag, or zero for another shape.
///
/// # Safety
/// `node` must be null or a live node from this runtime.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_rt_native_value_enum_tag(node: KNativeStateValue) -> u32 {
    if node.is_null() {
        return 0;
    }
    // SAFETY: the caller vouches the node is live.
    match unsafe { &(*node).value } {
        NodeValue::Ready(NativeStateValue::Enum { tag, .. }) => *tag,
        NodeValue::Aggregate { enum_tag, .. } => *enum_tag,
        _ => 0,
    }
}

/// Clones aggregate child `index` into a fresh ready node.
///
/// # Safety
/// `node` must be null or a live node from this runtime.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_rt_native_value_child(
    node: KNativeStateValue,
    index: usize,
) -> KNativeStateValue {
    if node.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: the caller vouches the node is live.
    match unsafe { &(*node).value } {
        NodeValue::Ready(NativeStateValue::Struct(values))
        | NodeValue::Ready(NativeStateValue::DropStruct { fields: values, .. })
        | NodeValue::Ready(NativeStateValue::Array(values)) => values
            .get(index)
            .map_or(std::ptr::null_mut(), |value| boxed(value.clone())),
        NodeValue::Ready(NativeStateValue::Enum { payload, .. }) if index == 0 => payload
            .as_deref()
            .map_or(std::ptr::null_mut(), |value| boxed(value.clone())),
        NodeValue::Ready(NativeStateValue::Any { payload, .. }) if index == 0 => {
            boxed(payload.as_ref().clone())
        }
        NodeValue::Ready(NativeStateValue::CBlock(block)) => block
            .children()
            .get(index)
            .map(|child| NativeStateValue::CBlock(child.block().clone()))
            .map_or(std::ptr::null_mut(), boxed),
        NodeValue::Aggregate { children, .. } => children
            .get(index)
            .and_then(Option::as_ref)
            .map_or(std::ptr::null_mut(), |value| boxed(value.clone())),
        NodeValue::CBlock { children, .. } => children
            .get(index)
            .and_then(Option::as_ref)
            .map(|child| NativeStateValue::CBlock(child.block.clone()))
            .map_or(std::ptr::null_mut(), boxed),
        _ => std::ptr::null_mut(),
    }
}

/// Releases one temporary value node. Null is a no-op.
///
/// # Safety
/// `node` must be null or one live node from this runtime and freed at most once.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_rt_native_value_free(node: KNativeStateValue) {
    if !node.is_null() {
        // SAFETY: the caller gives up exactly one live node.
        drop(unsafe { Box::from_raw(node) });
    }
}

mod state;
pub use state::*;

fn value_tag(value: &NativeStateValue) -> NativeStateValueTag {
    match value {
        NativeStateValue::Int(_) => NativeStateValueTag::INT,
        NativeStateValue::Float(_) => NativeStateValueTag::FLOAT,
        NativeStateValue::Bool(_) => NativeStateValueTag::BOOL,
        NativeStateValue::String(_) => NativeStateValueTag::STRING,
        NativeStateValue::Number(_) => NativeStateValueTag::NUMBER,
        NativeStateValue::Struct(_) => NativeStateValueTag::STRUCT,
        NativeStateValue::DropStruct { .. } => NativeStateValueTag::DROP_STRUCT,
        NativeStateValue::Array(_) => NativeStateValueTag::ARRAY,
        NativeStateValue::Enum { .. } => NativeStateValueTag::ENUM,
        NativeStateValue::RawPtr(_) => NativeStateValueTag::RAW_PTR,
        NativeStateValue::Cell(_) => NativeStateValueTag::CELL,
        NativeStateValue::Any { .. } => NativeStateValueTag::ANY,
        NativeStateValue::CBlock(_) => NativeStateValueTag::C_BLOCK,
        NativeStateValue::NativeState(_) => NativeStateValueTag::NATIVE_STATE,
    }
}

#[cfg(test)]
mod tests;
