//! Native callback-state bridge tests.

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
