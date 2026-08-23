//! Native-state store and value tests.

use super::*;

const LEFT: NativeStateTypeId = NativeStateTypeId::new(1);
const RIGHT: NativeStateTypeId = NativeStateTypeId::new(2);

#[test]
fn native_state_c_tags_and_statuses_are_pinned() {
    assert_eq!(NativeStateValueTag::INT.0, 1);
    assert_eq!(NativeStateValueTag::FLOAT.0, 2);
    assert_eq!(NativeStateValueTag::BOOL.0, 3);
    assert_eq!(NativeStateValueTag::STRING.0, 4);
    assert_eq!(NativeStateValueTag::STRUCT.0, 5);
    assert_eq!(NativeStateValueTag::ARRAY.0, 6);
    assert_eq!(NativeStateValueTag::ENUM.0, 7);
    assert_eq!(NativeStateValueTag::RAW_PTR.0, 8);
    assert_eq!(NativeStateValueTag::CELL.0, 9);
    assert_eq!(NativeStateValueTag::ANY.0, 10);
    assert_eq!(NativeStateStatus::OK.0, 0);
    assert_eq!(NativeStateStatus::NO_HOST.0, 1);
    assert_eq!(NativeStateStatus::NULL_TOKEN.0, 2);
    assert_eq!(NativeStateStatus::UNKNOWN_TOKEN.0, 3);
    assert_eq!(NativeStateStatus::WRONG_TYPE.0, 4);
    assert_eq!(NativeStateStatus::TOKEN_EXHAUSTED.0, 5);
    assert_eq!(NativeStateStatus::MALFORMED_VALUE.0, 6);
}

/// A cell share survives the deep clone a write forces, and is given back
/// exactly once when the last node holding it goes.
///
/// The write is the case worth pinning: `native_state_walk_mut` unshares
/// each level it passes through, which clones every sibling node — so a
/// cell node is duplicated by a write that has nothing to do with it. An
/// engine asked to count those clones would have to be told about each one;
/// counting them here makes the arithmetic exact.
#[test]
fn a_cell_share_is_returned_once_however_often_its_node_is_cloned() {
    let releases = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counted = Arc::clone(&releases);
    let root = NativeStateValue::struct_of(vec![
        NativeStateValue::Int(1),
        NativeStateValue::Cell(NativeCell::new(7, move |handle| {
            assert_eq!(handle, 7);
            counted.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        })),
    ]);

    // A reader holds the children, so the write below has to unshare them.
    let snapshot = root.clone();
    let mut written = root;
    let slot = native_state_walk_mut(&mut written, &[NativeStatePathStep::Field(0)])
        .expect("the first field is a value");
    *slot = NativeStateValue::Int(2);
    assert_eq!(
        releases.load(std::sync::atomic::Ordering::Relaxed),
        0,
        "a duplicated node is another share, not a release"
    );

    drop(snapshot);
    assert_eq!(releases.load(std::sync::atomic::Ordering::Relaxed), 0);
    drop(written);
    assert_eq!(
        releases.load(std::sync::atomic::Ordering::Relaxed),
        1,
        "the engine hears once, when the last share goes"
    );
}

/// Two shares of one cell are the same cell; the contents are not the
/// question, because a cell is a place to write.
#[test]
fn cell_nodes_compare_by_the_storage_they_name() {
    let left = NativeStateValue::Cell(NativeCell::new(3, |_| {}));
    let same = NativeStateValue::Cell(NativeCell::new(3, |_| {}));
    let other = NativeStateValue::Cell(NativeCell::new(4, |_| {}));
    assert_eq!(left, same);
    assert_ne!(left, other);
}

#[test]
fn any_nodes_keep_the_type_id_and_one_recursive_child() {
    let value = NativeStateValue::any_of(0x0005_0000_0000_0002, NativeStateValue::Int(7));
    let NativeStateValue::Any { type_id, .. } = &value else {
        panic!("the constructor must produce an Any node");
    };
    assert_eq!(*type_id, 0x0005_0000_0000_0002);
    assert_eq!(value.children().map(|children| children.len()), Some(1));
    assert_eq!(value.into_children(), Some(vec![NativeStateValue::Int(7)]));
}

#[test]
fn store_recovers_mutates_and_frees_once() {
    let mut store = NativeStateStore::new();
    let token = store
        .create(
            LEFT,
            NativeStateValue::struct_of(vec![NativeStateValue::Int(3)]),
        )
        .expect("state allocates");
    assert_ne!(token.as_word(), 0);
    assert_eq!(
        store.recover(token, LEFT),
        Ok(NativeStateValue::struct_of(vec![NativeStateValue::Int(3)]))
    );
    store
        .replace(
            token,
            LEFT,
            NativeStateValue::struct_of(vec![NativeStateValue::Int(7)]),
        )
        .expect("state mutates");
    assert_eq!(
        store.recover(token, LEFT),
        Ok(NativeStateValue::struct_of(vec![NativeStateValue::Int(7)]))
    );
    assert_eq!(store.free(token), Ok(()));
    assert_eq!(
        store.free(token),
        Err(NativeStateError::UnknownToken(token.as_word()))
    );
}

#[test]
fn callback_state_teardown_releases_a_nested_enum_cell_share() {
    let releases = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counted = Arc::clone(&releases);
    let value = NativeStateValue::struct_of(vec![NativeStateValue::enum_of(
        3,
        Some(NativeStateValue::Cell(NativeCell::new(23, move |handle| {
            assert_eq!(handle, 23);
            counted.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }))),
    )]);
    let mut store = NativeStateStore::new();
    let token = store.create(LEFT, value).expect("callback state allocates");
    let recovered = store.recover(token, LEFT).expect("callback state recovers");
    drop(recovered);
    assert_eq!(
        releases.load(std::sync::atomic::Ordering::Relaxed),
        0,
        "the stored state still owns its cell share"
    );
    store.free(token).expect("callback state frees");
    assert_eq!(
        releases.load(std::sync::atomic::Ordering::Relaxed),
        1,
        "final state teardown releases the nested enum cell once"
    );
}

#[test]
fn store_rejects_wrong_null_and_unknown_tokens() {
    let mut store = NativeStateStore::new();
    let token = store
        .create(LEFT, NativeStateValue::Int(1))
        .expect("state allocates");
    assert_eq!(
        store.recover(token, RIGHT),
        Err(NativeStateError::WrongType {
            actual: LEFT.as_word(),
            requested: RIGHT.as_word(),
        })
    );
    assert_eq!(
        store.recover(NativeStateToken::from_word(0), LEFT),
        Err(NativeStateError::NullToken)
    );
    assert_eq!(
        store.free(NativeStateToken::from_word(999)),
        Err(NativeStateError::UnknownToken(999))
    );
}
