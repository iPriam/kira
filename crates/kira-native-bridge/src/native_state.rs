//! Opaque process-lifetime native callback-state storage and value nodes.

use std::sync::{Mutex, OnceLock};

use kira_runtime_abi::{
    NativeCell, NativeStateStatus, NativeStateStore, NativeStateToken, NativeStateTypeId,
    NativeStateValue, NativeStateValueTag,
};

use crate::array::{
    ElemClone, KArray, kira_rt_array_free, kira_rt_array_len, kira_rt_array_new,
    kira_rt_array_slot, make_array_unique,
};
use crate::runtime::{KStr, kira_rt_str_data, kira_rt_str_free, kira_rt_str_len, kira_rt_str_new};

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
    }
}

/// Creates an integer state-value node.
#[unsafe(no_mangle)]
pub extern "C" fn kira_rt_native_value_int(value: i64) -> KNativeStateValue {
    boxed(NativeStateValue::Int(value))
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

/// Creates a capture-cell state-value node from a native cell box.
///
/// The share is taken here and given back when the node's last clone goes, so
/// the box outlives the crossing whatever either half does with it afterwards.
///
/// # A cell crosses as a handle, and only its own engine reads it
///
/// A captured `var` is one box two holders write through, and the two halves of
/// a hybrid program keep their values in separate storage — so a cell cannot be
/// *copied* across, the way a struct's fields are. It does not have to be. What
/// crosses is a closure's representation struct, and the closure's body runs on
/// the half that declared it: the other half carries the field and hands it
/// back, exactly as it carries a `RawPtr` whose target means nothing to it.
///
/// That is the whole contract. A half that tried to *read* a cell the other one
/// created would be reading storage it does not own, and nothing generates that:
/// a `CellGet` is emitted where the binding was declared.
///
/// # SAFETY
///
/// `cell` is a live cell box this runtime allocated, and the caller keeps its
/// own share.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_rt_native_value_cell(cell: crate::cells::KCell) -> KNativeStateValue {
    // SAFETY: the caller's contract is a live box, and a cell box *is* an enum
    // box — the clone is its share count going up.
    let shared = unsafe { crate::enums::kira_rt_enum_clone(cell) };
    boxed(NativeStateValue::Cell(NativeCell::new(
        shared as u64,
        |handle| {
            // SAFETY: the share this releases is the one taken above, and it is
            // released exactly once — the node counts its own clones.
            unsafe { crate::cells::kira_rt_cell_free(handle as crate::cells::KCell) };
        },
    )))
}

/// Reads the cell box out of a capture-cell node, keeping the node's share.
///
/// # SAFETY
///
/// `node` is a live node this runtime allocated.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_rt_native_value_read_cell(
    node: KNativeStateValue,
) -> crate::cells::KCell {
    if node.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: the caller vouches the node is live.
    match unsafe { &(*node).value } {
        NodeValue::Ready(NativeStateValue::Cell(cell)) => {
            // The reader gets a share of its own: the node keeps counting the
            // one it took, and a decode hands its result to an owner.
            // SAFETY: the node holds a live box, so cloning it is sound.
            unsafe { crate::enums::kira_rt_enum_clone(cell.handle() as crate::cells::KCell) }
        }
        _ => std::ptr::null_mut(),
    }
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
        | NodeValue::Ready(NativeStateValue::Array(values)) => values.len(),
        NodeValue::Ready(NativeStateValue::Enum { payload, .. }) => usize::from(payload.is_some()),
        NodeValue::Ready(NativeStateValue::Any { .. }) => 1,
        NodeValue::Aggregate { children, .. } => children.len(),
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
    let value = match unsafe { &(*node).value } {
        NodeValue::Ready(NativeStateValue::Struct(values))
        | NodeValue::Ready(NativeStateValue::Array(values)) => values.get(index),
        NodeValue::Ready(NativeStateValue::Enum { payload, .. }) if index == 0 => {
            payload.as_deref()
        }
        NodeValue::Ready(NativeStateValue::Any { payload, .. }) if index == 0 => {
            Some(payload.as_ref())
        }
        NodeValue::Aggregate { children, .. } => children.get(index).and_then(Option::as_ref),
        _ => None,
    };
    value.map_or(std::ptr::null_mut(), |value| boxed(value.clone()))
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

/// Boxes a completed value node and writes its stable token to `out`.
///
/// # Safety
/// `value` must be one live node the call consumes, and `out` must be writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_rt_native_state_new(
    type_id: u64,
    value: KNativeStateValue,
    out: *mut u64,
) -> u32 {
    if out.is_null() {
        return NativeStateStatus::MALFORMED_VALUE.0;
    }
    let value = match finish(value) {
        Ok(value) => value,
        Err(status) => return status.0,
    };
    let mut store = match store().lock() {
        Ok(store) => store,
        Err(poisoned) => poisoned.into_inner(),
    };
    match store.create(NativeStateTypeId::new(type_id), value) {
        Ok(token) => {
            // SAFETY: the caller supplies one writable word.
            unsafe { *out = token.as_word() };
            NativeStateStatus::OK.0
        }
        Err(error) => status(error),
    }
}

/// Recovers a typed owned copy into `out`.
///
/// # Safety
/// `out` must be writable when non-null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_rt_native_state_recover(
    token: u64,
    type_id: u64,
    out: *mut KNativeStateValue,
) -> u32 {
    if out.is_null() {
        return NativeStateStatus::MALFORMED_VALUE.0;
    }
    let store = match store().lock() {
        Ok(store) => store,
        Err(poisoned) => poisoned.into_inner(),
    };
    match store.recover(
        NativeStateToken::from_word(token),
        NativeStateTypeId::new(type_id),
    ) {
        Ok(value) => {
            // SAFETY: the caller supplies one writable pointer slot.
            unsafe { *out = boxed(value) };
            NativeStateStatus::OK.0
        }
        Err(error) => status(error),
    }
}

/// Replaces typed state with a completed value node.
///
/// # Safety
/// `value` must be one live node the call consumes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_rt_native_state_replace(
    token: u64,
    type_id: u64,
    value: KNativeStateValue,
) -> u32 {
    let value = match finish(value) {
        Ok(value) => value,
        Err(status) => return status.0,
    };
    let mut store = match store().lock() {
        Ok(store) => store,
        Err(poisoned) => poisoned.into_inner(),
    };
    match store.replace(
        NativeStateToken::from_word(token),
        NativeStateTypeId::new(type_id),
        value,
    ) {
        Ok(()) => NativeStateStatus::OK.0,
        Err(error) => status(error),
    }
}

/// Releases one state token exactly once.
#[unsafe(no_mangle)]
pub extern "C" fn kira_rt_native_state_free(token: u64) -> u32 {
    // One release path for both kinds of state, told apart by the token itself:
    // a native engine's state is a box it owns, and the value-tree store never
    // hands out an odd token. See `crate::state_box`.
    if crate::state_box::is_box_token(token) {
        // SAFETY: the token is a box token this runtime handed out, and a
        // caller releases it once — the same contract this function already had.
        return unsafe { crate::state_box::kira_rt_native_state_box_free(token) };
    }
    let mut store = match store().lock() {
        Ok(store) => store,
        Err(poisoned) => poisoned.into_inner(),
    };
    match store.free(NativeStateToken::from_word(token)) {
        Ok(()) => NativeStateStatus::OK.0,
        Err(error) => status(error),
    }
}

/// Terminates native execution with a deterministic callback-state trap.
#[unsafe(no_mangle)]
pub extern "C" fn kira_rt_trap_native_state(status: u32) -> ! {
    let message = match NativeStateStatus(status) {
        NativeStateStatus::NO_HOST => "no callback-state host",
        NativeStateStatus::NULL_TOKEN => "null callback-state token",
        NativeStateStatus::UNKNOWN_TOKEN => "unknown or already-freed callback-state token",
        NativeStateStatus::WRONG_TYPE => "callback-state type mismatch",
        NativeStateStatus::TOKEN_EXHAUSTED => "callback-state token space exhausted",
        NativeStateStatus::MALFORMED_VALUE => "malformed callback-state value",
        _ => "unknown callback-state failure",
    };
    eprintln!("kira: runtime trap: {message}");
    std::process::exit(1)
}

fn value_tag(value: &NativeStateValue) -> NativeStateValueTag {
    match value {
        NativeStateValue::Int(_) => NativeStateValueTag::INT,
        NativeStateValue::Float(_) => NativeStateValueTag::FLOAT,
        NativeStateValue::Bool(_) => NativeStateValueTag::BOOL,
        NativeStateValue::String(_) => NativeStateValueTag::STRING,
        NativeStateValue::Struct(_) => NativeStateValueTag::STRUCT,
        NativeStateValue::Array(_) => NativeStateValueTag::ARRAY,
        NativeStateValue::Enum { .. } => NativeStateValueTag::ENUM,
        NativeStateValue::RawPtr(_) => NativeStateValueTag::RAW_PTR,
        NativeStateValue::Cell(_) => NativeStateValueTag::CELL,
        NativeStateValue::Any { .. } => NativeStateValueTag::ANY,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::enums::PAYLOAD_INERT;

    #[test]
    fn native_store_mutates_and_rejects_invalid_tokens() {
        let node = kira_rt_native_value_aggregate(NativeStateValueTag::STRUCT.0, 0, 1);
        let child = kira_rt_native_value_int(4);
        // SAFETY: both nodes are live and slot zero exists.
        assert_eq!(unsafe { kira_rt_native_value_set_child(node, 0, child) }, 0);
        let mut token = 0;
        // SAFETY: `node` is live and `token` is writable.
        assert_eq!(unsafe { kira_rt_native_state_new(7, node, &mut token) }, 0);
        assert_ne!(token, 0);

        let mut recovered = std::ptr::null_mut();
        // SAFETY: `recovered` is writable.
        let recovered_status = unsafe { kira_rt_native_state_recover(token, 7, &mut recovered) };
        assert_eq!(recovered_status, 0);
        // SAFETY: `recovered` is a live struct node.
        let child = unsafe { kira_rt_native_value_child(recovered, 0) };
        // SAFETY: `child` is a live integer node.
        assert_eq!(unsafe { kira_rt_native_value_read_int(child) }, 4);
        // SAFETY: both temporary nodes are live and uniquely owned.
        unsafe {
            kira_rt_native_value_free(child);
            kira_rt_native_value_free(recovered);
        }

        assert_eq!(kira_rt_native_state_free(token), 0);
        assert_eq!(
            kira_rt_native_state_free(token),
            NativeStateStatus::UNKNOWN_TOKEN.0
        );
        let mut out = std::ptr::null_mut();
        // SAFETY: `out` is writable.
        let null_status = unsafe { kira_rt_native_state_recover(0, 7, &mut out) };
        assert_eq!(null_status, NativeStateStatus::NULL_TOKEN.0);
        // SAFETY: `out` is writable.
        let unknown_status = unsafe { kira_rt_native_state_recover(999_999, 7, &mut out) };
        assert_eq!(unknown_status, NativeStateStatus::UNKNOWN_TOKEN.0);
    }

    #[test]
    fn native_store_rejects_wrong_type() {
        let node = kira_rt_native_value_int(1);
        let mut token = 0;
        // SAFETY: `node` is live and `token` is writable.
        assert_eq!(unsafe { kira_rt_native_state_new(11, node, &mut token) }, 0);
        let mut out = std::ptr::null_mut();
        // SAFETY: `out` is writable.
        let status = unsafe { kira_rt_native_state_recover(token, 12, &mut out) };
        assert_eq!(status, NativeStateStatus::WRONG_TYPE.0);
        assert_eq!(kira_rt_native_state_free(token), 0);
    }

    #[test]
    fn any_and_cell_nodes_keep_their_recursive_value_and_share() {
        let child = kira_rt_native_value_int(7);
        // SAFETY: `child` is live and ownership moves into the Any node.
        let any = unsafe { kira_rt_native_value_any(0x0500_0000_0000_0001, child) };
        // SAFETY: `any` is the live node built above.
        let (tag, type_id, len) = unsafe {
            (
                kira_rt_native_value_tag(any),
                kira_rt_native_value_read_any_type(any),
                kira_rt_native_value_len(any),
            )
        };
        assert_eq!(tag, NativeStateValueTag::ANY.0);
        assert_eq!(type_id, 0x0500_0000_0000_0001);
        assert_eq!(len, 1);
        // SAFETY: `any` is live and its child is returned as a fresh node.
        let child = unsafe { kira_rt_native_value_child(any, 0) };
        // SAFETY: `child` is that node, which carries the integer read here.
        let value = unsafe { kira_rt_native_value_read_int(child) };
        assert_eq!(value, 7);
        // SAFETY: both nodes are live and released once.
        unsafe {
            kira_rt_native_value_free(child);
            kira_rt_native_value_free(any);
        }

        let cell = crate::cells::kira_rt_cell_new(PAYLOAD_INERT, 9);
        // SAFETY: the cell is live, and the node takes one additional share.
        let node = unsafe { kira_rt_native_value_cell(cell) };
        // SAFETY: `node` is that live cell node.
        let tag = unsafe { kira_rt_native_value_tag(node) };
        assert_eq!(tag, NativeStateValueTag::CELL.0);
        // SAFETY: `node` holds a live cell share and the read takes another.
        let read = unsafe { kira_rt_native_value_read_cell(node) };
        assert_eq!(read, cell);
        // SAFETY: every cell share and the node are live and released once.
        unsafe {
            crate::cells::kira_rt_cell_free(read);
            crate::cells::kira_rt_cell_free(cell);
            kira_rt_native_value_free(node);
        }
    }

    #[test]
    fn callback_state_teardown_releases_a_nested_enum_cell_once() {
        let before = crate::accounting::kira_rt_heap_live();
        let cell = crate::cells::kira_rt_cell_new(PAYLOAD_INERT, 23);
        // SAFETY: every node is live, and ownership moves into its parent on
        // each successful child write.
        let cell_node = unsafe { kira_rt_native_value_cell(cell) };
        let enum_node = kira_rt_native_value_aggregate(NativeStateValueTag::ENUM.0, 3, 1);
        // SAFETY: both nodes are live and the child moves into its parent.
        let status = unsafe { kira_rt_native_value_set_child(enum_node, 0, cell_node) };
        assert_eq!(status, NativeStateStatus::OK.0);
        let root = kira_rt_native_value_aggregate(NativeStateValueTag::STRUCT.0, 0, 1);
        // SAFETY: as above, for the root and the enum node it takes.
        let status = unsafe { kira_rt_native_value_set_child(root, 0, enum_node) };
        assert_eq!(status, NativeStateStatus::OK.0);

        let mut token = 0;
        // SAFETY: `root` is consumed and `token` is writable.
        let status = unsafe { kira_rt_native_state_new(19, root, &mut token) };
        assert_eq!(status, NativeStateStatus::OK.0);
        // The state node retained its own cell share.
        // SAFETY: this releases only the share this test created.
        unsafe { crate::cells::kira_rt_cell_free(cell) };

        let mut recovered = std::ptr::null_mut();
        // SAFETY: `recovered` is writable and `token` names the live state.
        let status = unsafe { kira_rt_native_state_recover(token, 19, &mut recovered) };
        assert_eq!(status, NativeStateStatus::OK.0);
        // SAFETY: the recovered tree is owned by this test, and each child is
        // returned as a fresh node whose cell read takes its own share.
        let (recovered_enum, recovered_cell, read, value) = unsafe {
            let recovered_enum = kira_rt_native_value_child(recovered, 0);
            let recovered_cell = kira_rt_native_value_child(recovered_enum, 0);
            let read = kira_rt_native_value_read_cell(recovered_cell);
            let value = crate::cells::kira_rt_cell_get(read);
            (recovered_enum, recovered_cell, read, value)
        };
        assert_eq!(value, 23);
        // SAFETY: the read and all recovered nodes are owned by this test.
        unsafe {
            crate::cells::kira_rt_cell_free(read);
            kira_rt_native_value_free(recovered_cell);
            kira_rt_native_value_free(recovered_enum);
            kira_rt_native_value_free(recovered);
        }
        assert_eq!(kira_rt_native_state_free(token), NativeStateStatus::OK.0);
        assert_eq!(
            crate::accounting::kira_rt_heap_live(),
            before,
            "state recovery and final teardown release the enum cell"
        );
    }
}
