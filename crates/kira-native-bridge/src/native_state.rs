//! Opaque process-lifetime native callback-state storage and value nodes.

use std::sync::{Arc, Mutex, OnceLock};

use kira_runtime_abi::{
    NativeStateStatus, NativeStateStore, NativeStateToken, NativeStateTypeId, NativeStateValue,
    NativeStateValueTag,
};

use crate::array::{
    KArray, kira_rt_array_free, kira_rt_array_len, kira_rt_array_new, kira_rt_array_slot,
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
    /// A read-only window onto live state: the whole tree, plus the steps
    /// walked into it.
    ///
    /// Recovering state hands one of these out instead of a copy, and walking
    /// into it appends a step instead of copying a subtree. Reading one field
    /// of a compositor's batch state used to copy its glyph cache; now it
    /// copies nothing. The tree stays valid however the state is replaced,
    /// because replacing swaps the handle rather than writing through it — so
    /// a view reads what the state held when it was recovered.
    View {
        root: Arc<NativeStateValue>,
        path: Vec<usize>,
    },
    Aggregate {
        tag: NativeStateValueTag,
        enum_tag: u32,
        children: Vec<Option<NativeStateValue>>,
    },
}

impl NativeStateNode {
    /// The value this node reads, when it is one a reader can reach.
    ///
    /// An `Aggregate` is under construction and has no value yet, which is why
    /// this is an option rather than a reference.
    fn value(&self) -> Option<&NativeStateValue> {
        match &self.value {
            NodeValue::Ready(value) => Some(value),
            NodeValue::View { root, path } => walk(root, path),
            NodeValue::Aggregate { .. } => None,
        }
    }
}

/// Follows `path` from `root`, or `None` when a step leads nowhere.
fn walk<'a>(root: &'a NativeStateValue, path: &[usize]) -> Option<&'a NativeStateValue> {
    let mut at = root;
    for &step in path {
        at = child_of(at, step)?;
    }
    Some(at)
}

/// The child an aggregate holds at `index`, or `None` for another shape.
fn child_of(value: &NativeStateValue, index: usize) -> Option<&NativeStateValue> {
    match value {
        NativeStateValue::Struct(values) | NativeStateValue::Array(values) => values.get(index),
        NativeStateValue::Enum { payload, .. } if index == 0 => payload.as_deref(),
        _ => None,
    }
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
        // A view is a window on state someone else owns, so handing it on as an
        // owned value is the one place it has to be copied.
        NodeValue::View { ref root, ref path } => walk(root, path)
            .cloned()
            .ok_or(NativeStateStatus::MALFORMED_VALUE),
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
                NativeStateValueTag::STRUCT => NativeStateValue::Struct(values),
                NativeStateValueTag::ARRAY => NativeStateValue::Array(values),
                NativeStateValueTag::ENUM => {
                    if values.len() > 1 {
                        return Err(NativeStateStatus::MALFORMED_VALUE);
                    }
                    NativeStateValue::Enum {
                        tag: enum_tag,
                        payload: values.pop().map(Box::new),
                    }
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

/// Creates an opaque raw-pointer-word state-value node.
#[unsafe(no_mangle)]
pub extern "C" fn kira_rt_native_value_raw_ptr(value: u64) -> KNativeStateValue {
    boxed(NativeStateValue::RawPtr(value))
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
/// # Safety
/// `array` must be a live owned array with element size `esize`; `encode` must
/// consume one element from each slot and return one live node.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_rt_native_value_array_from(
    array: KArray,
    esize: usize,
    encode: NativeStateEncodeElement,
) -> KNativeStateValue {
    // SAFETY: the caller vouches the array is live.
    let len = unsafe { kira_rt_array_len(array) };
    let count = usize::try_from(len).unwrap_or(0);
    let node = kira_rt_native_value_aggregate(NativeStateValueTag::ARRAY.0, 0, count);
    for index in 0..count {
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
    if let NodeValue::Aggregate { tag, .. } = unsafe { &(*node).value } {
        return tag.0;
    }
    // SAFETY: same live node.
    unsafe { (*node).value() }.map_or(0, |value| value_tag(value).0)
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
    match unsafe { (*node).value() } {
        Some(NativeStateValue::Int(value)) => *value,
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
    match unsafe { (*node).value() } {
        Some(NativeStateValue::RawPtr(value)) => *value,
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
    match unsafe { (*node).value() } {
        Some(NativeStateValue::Float(value)) => *value,
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
    match unsafe { (*node).value() } {
        Some(NativeStateValue::Bool(value)) => u8::from(*value),
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
    let Some(NativeStateValue::String(value)) = (unsafe { (*node).value() }) else {
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
    if let NodeValue::Aggregate { children, .. } = unsafe { &(*node).value } {
        return children.len();
    }
    // SAFETY: same live node.
    match unsafe { (*node).value() } {
        Some(NativeStateValue::Struct(values) | NativeStateValue::Array(values)) => values.len(),
        Some(NativeStateValue::Enum { payload, .. }) => usize::from(payload.is_some()),
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
    if let NodeValue::Aggregate { enum_tag, .. } = unsafe { &(*node).value } {
        return *enum_tag;
    }
    // SAFETY: same live node.
    match unsafe { (*node).value() } {
        Some(NativeStateValue::Enum { tag, .. }) => *tag,
        _ => 0,
    }
}

/// Returns aggregate child `index` as a fresh node.
///
/// A view walks: the child node names the same tree one step deeper, and
/// nothing is copied. Any other node has to hand out a copy, because it owns
/// its value and the child would outlive the borrow.
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
    if let NodeValue::View { root, path } = unsafe { &(*node).value } {
        let mut path = path.clone();
        path.push(index);
        if walk(root, &path).is_none() {
            return std::ptr::null_mut();
        }
        return Box::into_raw(Box::new(NativeStateNode {
            value: NodeValue::View {
                root: Arc::clone(root),
                path,
            },
        }));
    }
    // SAFETY: same live node.
    let value = match unsafe { &(*node).value } {
        NodeValue::Ready(value) => child_of(value, index),
        NodeValue::Aggregate { children, .. } => children.get(index).and_then(Option::as_ref),
        NodeValue::View { .. } => None,
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

/// Recovers a typed read-only view of the live state into `out`.
///
/// A view rather than a copy: the caller reads fields out of it and builds a
/// fresh value to replace the state with, and copying the tree first is work
/// nothing reads. The view holds the tree it was recovered from, so a later
/// replace neither disturbs it nor is disturbed by it.
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
    match store.recover_shared(
        NativeStateToken::from_word(token),
        NativeStateTypeId::new(type_id),
    ) {
        Ok(root) => {
            let node = NativeStateNode {
                value: NodeValue::View {
                    root,
                    path: Vec::new(),
                },
            };
            // SAFETY: the caller supplies one writable pointer slot.
            unsafe { *out = Box::into_raw(Box::new(node)) };
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
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    /// A recovered view reads the state as it was when it was recovered, and
    /// replacing the state neither disturbs it nor is disturbed by it.
    ///
    /// That was true of the copy this replaced, and it has to stay true: a
    /// generated function recovers, reads its fields, and replaces — reading a
    /// field *after* the replace and seeing the new value would change what
    /// the code around it means.
    #[test]
    fn a_recovered_view_is_a_snapshot_a_replace_does_not_move() {
        let node = kira_rt_native_value_aggregate(NativeStateValueTag::STRUCT.0, 0, 1);
        let before = kira_rt_native_value_int(1);
        // SAFETY: both nodes are live and slot zero exists.
        let set = unsafe { kira_rt_native_value_set_child(node, 0, before) };
        assert_eq!(set, 0);
        let mut token = 0;
        // SAFETY: `node` is live and `token` is writable.
        assert_eq!(unsafe { kira_rt_native_state_new(21, node, &mut token) }, 0);

        let mut view = std::ptr::null_mut();
        // SAFETY: `view` is writable.
        let recovered = unsafe { kira_rt_native_state_recover(token, 21, &mut view) };
        assert_eq!(recovered, 0);

        let replacement = kira_rt_native_value_aggregate(NativeStateValueTag::STRUCT.0, 0, 1);
        let after = kira_rt_native_value_int(2);
        // SAFETY: both nodes are live and slot zero exists.
        let set = unsafe { kira_rt_native_value_set_child(replacement, 0, after) };
        assert_eq!(set, 0);
        // SAFETY: `replacement` is live and given up here.
        let replaced = unsafe { kira_rt_native_state_replace(token, 21, replacement) };
        assert_eq!(replaced, 0);

        // SAFETY: the view is still live and still names a struct.
        let child = unsafe { kira_rt_native_value_child(view, 0) };
        // SAFETY: `child` is a live integer node.
        assert_eq!(unsafe { kira_rt_native_value_read_int(child) }, 1);
        // SAFETY: both temporary nodes are live and uniquely owned.
        unsafe {
            kira_rt_native_value_free(child);
            kira_rt_native_value_free(view);
        }

        let mut fresh = std::ptr::null_mut();
        // SAFETY: `fresh` is writable.
        let recovered = unsafe { kira_rt_native_state_recover(token, 21, &mut fresh) };
        assert_eq!(recovered, 0);
        // SAFETY: `fresh` is a live struct node.
        let child = unsafe { kira_rt_native_value_child(fresh, 0) };
        // SAFETY: `child` is a live integer node.
        assert_eq!(unsafe { kira_rt_native_value_read_int(child) }, 2);
        // SAFETY: both temporary nodes are live and uniquely owned.
        unsafe {
            kira_rt_native_value_free(child);
            kira_rt_native_value_free(fresh);
        }
        assert_eq!(kira_rt_native_state_free(token), 0);
    }

    /// Walking a view reports a missing child rather than inventing one.
    #[test]
    fn a_view_step_past_the_end_is_null() {
        let node = kira_rt_native_value_aggregate(NativeStateValueTag::STRUCT.0, 0, 1);
        let only = kira_rt_native_value_int(5);
        // SAFETY: both nodes are live and slot zero exists.
        assert_eq!(unsafe { kira_rt_native_value_set_child(node, 0, only) }, 0);
        let mut token = 0;
        // SAFETY: `node` is live and `token` is writable.
        assert_eq!(unsafe { kira_rt_native_state_new(22, node, &mut token) }, 0);
        let mut view = std::ptr::null_mut();
        // SAFETY: `view` is writable.
        let recovered = unsafe { kira_rt_native_state_recover(token, 22, &mut view) };
        assert_eq!(recovered, 0);
        // SAFETY: the view is live; index one is past its only child.
        assert!(unsafe { kira_rt_native_value_child(view, 1) }.is_null());
        // SAFETY: the view is live and uniquely owned.
        unsafe { kira_rt_native_value_free(view) };
        assert_eq!(kira_rt_native_state_free(token), 0);
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
}
