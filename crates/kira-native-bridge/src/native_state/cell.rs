//! Capture-cell native-state node transport.

use super::*;

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
