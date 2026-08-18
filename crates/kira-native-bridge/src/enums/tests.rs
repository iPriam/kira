//! Tests for the enum bridge.
//!
//! In their own file because they are the larger half: the bridge itself is a
//! handful of layout rules, and proving them takes one case per shape — inline
//! tags, payloads, nesting, and the allocation each one does or does not do.

use super::*;

/// Builds a string handle, as the backend's lowering would.
fn str_handle(text: &str) -> KStr {
    // SAFETY: the slice covers exactly `len` readable bytes.
    unsafe { crate::runtime::kira_rt_str_new(text.as_ptr(), text.len()) }
}

#[test]
fn a_scalar_enum_round_trips_its_tag_and_frees_cleanly() {
    // SAFETY: the handle is live and released once per copy of it.
    unsafe {
        let value = kira_rt_enum_new(2, PAYLOAD_INERT, 42);
        assert_eq!(kira_rt_enum_tag(value), 2);
        let copy = kira_rt_enum_clone(value);
        assert_eq!(kira_rt_enum_tag(copy), 2);
        assert_eq!(value, copy, "a copy is the same box, held twice");
        assert_eq!((*value).shares, 2);
        kira_rt_enum_free(value);
        kira_rt_enum_free(copy);
    }
}

/// A copy holds the box rather than duplicating its payload, and the payload
/// outlives every hold but the last.
///
/// Under Miri or ASan, releasing the payload with the first hold would
/// surface as a use-after-free on the read below; never releasing it would
/// surface as a leak.
#[test]
fn a_string_payload_is_shared_and_freed_with_the_last_hold() {
    // SAFETY: every handle below is live and released once per hold.
    unsafe {
        let value = kira_rt_enum_new(0, PAYLOAD_STR, str_handle("payload") as u64);
        let copy = kira_rt_enum_clone(value);
        assert_eq!(value, copy, "a copy is the same box");

        kira_rt_enum_free(value);
        let read = kira_rt_enum_payload(copy) as KStr;
        assert_eq!(
            crate::runtime::kira_rt_str_len(read),
            7,
            "the payload survived the first hold"
        );
        kira_rt_str_free(read);
        kira_rt_enum_free(copy);
    }
}

#[test]
fn a_payload_read_is_owned_and_leaves_the_enum_intact() {
    // What a `match` binding does: read the payload, then free the enum.
    // The read string must survive that, and freeing it must not double-free
    // the box's own — the affine guarantee the VM proves with heap counters.
    // SAFETY: every handle below is live and freed exactly once.
    unsafe {
        let value = kira_rt_enum_new(1, PAYLOAD_STR, str_handle("bound") as u64);
        let read = kira_rt_enum_payload(value) as KStr;
        // The read is the caller's to release, which is what matters — a
        // string is shared rather than duplicated, so it *is* the box's
        // handle, held once more. Releasing the box must leave it readable.
        kira_rt_enum_free(value);
        assert_eq!(
            crate::runtime::kira_rt_str_len(read),
            5,
            "the binding outlived the enum it came from"
        );
        kira_rt_str_free(read);

        let scalar = kira_rt_enum_new(0, PAYLOAD_INERT, 77);
        assert_eq!(kira_rt_enum_payload(scalar), 77);
        kira_rt_enum_free(scalar);
    }
}

#[repr(C)]
struct AggregateFixture {
    count: i64,
    label: KStr,
}

unsafe extern "C" fn clone_fixture(source: *const u8, target: *mut u8) {
    // SAFETY: test passes pointers to aligned `AggregateFixture` values.
    let (source, target) = unsafe {
        (
            &*(source.cast::<AggregateFixture>()),
            &mut *(target.cast::<AggregateFixture>()),
        )
    };
    // SAFETY: source label remains live for the duration of this clone.
    target.label = unsafe { kira_rt_str_clone(source.label) };
}

unsafe extern "C" fn free_fixture(value: *mut u8) {
    // SAFETY: test passes an aligned `AggregateFixture` slot exactly once.
    let value = unsafe { &mut *value.cast::<AggregateFixture>() };
    // SAFETY: the fixture owns this live label exactly once.
    unsafe { kira_rt_str_free(value.label) };
}

#[test]
fn a_struct_payload_is_read_out_independently_and_freed_with_its_box() {
    // SAFETY: every erased pointer uses `AggregateFixture`'s layout and every
    // owned handle is released exactly as many times as it is held.
    unsafe {
        let source = AggregateFixture {
            count: 7,
            label: str_handle("boxed"),
        };
        let value = kira_rt_enum_new_aggregate(
            3,
            std::ptr::from_ref(&source).cast::<u8>(),
            size_of::<AggregateFixture>(),
            Some(clone_fixture),
            Some(free_fixture),
        );
        let copy = kira_rt_enum_clone(value);
        assert_eq!(value, copy, "a copy is the same box");

        // A read *is* a copy, and the clone leaf is what makes it one: the
        // read takes a hold of the label, so it outlives the box below.
        let mut read = std::mem::MaybeUninit::<AggregateFixture>::uninit();
        kira_rt_enum_payload_aggregate(value, read.as_mut_ptr().cast::<u8>());
        let read = read.assume_init();
        assert_eq!(read.count, 7);
        kira_rt_enum_free(value);
        kira_rt_enum_free(copy);
        assert_eq!(crate::runtime::kira_rt_str_len(read.label), 5);
        kira_rt_str_free(read.label);
    }
}

#[repr(C)]
struct ArrayAggregateFixture {
    values: crate::array::KArray,
}

unsafe extern "C" fn free_string_element(value: *mut u8) {
    // SAFETY: the fixture's array stores one KStr per element.
    let value = unsafe { *value.cast::<KStr>() };
    // SAFETY: the element owns this string handle exactly once.
    unsafe { kira_rt_str_free(value) };
}

unsafe extern "C" fn clone_array_fixture(source: *const u8, target: *mut u8) {
    // SAFETY: test passes pointers to aligned ArrayAggregateFixture values.
    let (source, target) = unsafe {
        (
            &*source.cast::<ArrayAggregateFixture>(),
            &mut *target.cast::<ArrayAggregateFixture>(),
        )
    };
    // SAFETY: the source array is live and the target takes one new hold.
    target.values = unsafe { crate::array::kira_rt_array_clone(source.values) };
}

unsafe extern "C" fn free_array_fixture(value: *mut u8) {
    // SAFETY: test passes an aligned ArrayAggregateFixture slot exactly once.
    let value = unsafe { &mut *value.cast::<ArrayAggregateFixture>() };
    // SAFETY: the array owns one KStr per slot and is released once here.
    unsafe {
        crate::array::kira_rt_array_free(value.values, size_of::<KStr>(), Some(free_string_element))
    };
}

#[test]
fn an_array_payload_keeps_nested_strings_alive_across_read_and_drop() {
    // SAFETY: every array, string, and enum hold below is released exactly
    // once, and each erased pointer uses ArrayAggregateFixture's layout.
    unsafe {
        let values = crate::array::kira_rt_array_new(2, size_of::<KStr>());
        for (index, text) in ["one", "two"].into_iter().enumerate() {
            let slot = crate::array::kira_rt_array_slot(
                values,
                i64::try_from(index).expect("a fixture index fits in Int"),
                size_of::<KStr>(),
            );
            slot.cast::<KStr>().write(str_handle(text));
        }
        let source = ArrayAggregateFixture { values };
        let value = kira_rt_enum_new_aggregate(
            4,
            std::ptr::from_ref(&source).cast::<u8>(),
            size_of::<ArrayAggregateFixture>(),
            Some(clone_array_fixture),
            Some(free_array_fixture),
        );
        let copy = kira_rt_enum_clone(value);
        let mut read = std::mem::MaybeUninit::<ArrayAggregateFixture>::uninit();
        kira_rt_enum_payload_aggregate(value, read.as_mut_ptr().cast::<u8>());
        let read = read.assume_init();
        kira_rt_enum_free(value);
        kira_rt_enum_free(copy);
        assert_eq!(crate::array::kira_rt_array_len(read.values), 2);
        let first = crate::array::kira_rt_array_slot(read.values, 0, size_of::<KStr>())
            .cast::<KStr>()
            .read();
        assert_eq!(crate::runtime::kira_rt_str_len(first), 3);
        crate::array::kira_rt_array_free(read.values, size_of::<KStr>(), Some(free_string_element));
    }
}

/// The box is `#[repr(C)]`, so its layout is pinned here beside it. The
/// share count is last, leaving the three fields before it where they were.
#[test]
fn the_enum_box_layout_is_pinned() {
    assert_eq!(size_of::<KiraEnum>(), 32);
    assert_eq!(align_of::<KiraEnum>(), 8);
    assert_eq!(size_of::<KEnum>(), size_of::<usize>());
    let box_ = KiraEnum {
        tag: 0,
        payload_kind: 0,
        payload: 0,
        shares: 1,
    };
    let base = std::ptr::from_ref(&box_).cast::<u8>();
    // SAFETY: every field belongs to `box_`, which outlives the reads.
    unsafe {
        assert_eq!(
            std::ptr::from_ref(&box_.tag).cast::<u8>().offset_from(base),
            0
        );
        assert_eq!(
            std::ptr::from_ref(&box_.payload_kind)
                .cast::<u8>()
                .offset_from(base),
            8
        );
        assert_eq!(
            std::ptr::from_ref(&box_.payload)
                .cast::<u8>()
                .offset_from(base),
            16
        );
        // The backend GEPs this field rather than calling a helper for it,
        // so where it sits is a contract with separately compiled code.
        assert_eq!(
            std::ptr::from_ref(&box_.shares)
                .cast::<u8>()
                .offset_from(base),
            isize::try_from(kira_runtime_abi::ENUM_BOX_SHARES_FIELD).expect("a small index") * 8
        );
    }
}

/// A nested enum payload — what a `Result`-shaped `Error` variant carries —
/// is held, not duplicated, and released exactly once with the last hold.
///
/// Under Miri or ASan an over-release of the inner box would surface here
/// as a use-after-free, and a missed one as a leak.
#[test]
fn a_nested_enum_payload_is_held_and_released_with_its_owner() {
    // SAFETY: every handle below is live and released once per hold.
    unsafe {
        // `Error(.MissingNode("boom"))`: an enum whose payload is an enum
        // whose payload is a string — two levels of nesting.
        let inner = kira_rt_enum_new(1, PAYLOAD_STR, str_handle("boom") as u64);
        let outer = kira_rt_enum_new(0, PAYLOAD_ENUM, inner as u64);

        let copy = kira_rt_enum_clone(outer);
        assert_eq!(outer, copy, "a copy is the same box");
        assert_eq!(kira_rt_enum_tag((*copy).payload as KEnum), 1);

        // A payload read is a hold of its own, so releasing the outer twice
        // over must leave it valid.
        let read = kira_rt_enum_payload(outer) as KEnum;
        assert_eq!(read as u64, (*outer).payload, "the read holds the same box");
        assert_eq!((*inner).shares, 2);
        kira_rt_enum_free(outer);
        kira_rt_enum_free(copy);
        assert_eq!(kira_rt_enum_tag(read), 1, "the read survives its source");
        kira_rt_enum_free(read);
    }
}

#[test]
fn a_cell_enum_payload_clone_and_free_releases_the_nested_box() {
    let before = crate::accounting::kira_rt_heap_live();
    // SAFETY: the outer enum takes ownership of `cell`; the payload read
    // is an additional hold, and every hold is released below.
    unsafe {
        let cell = crate::cells::kira_rt_cell_new(PAYLOAD_INERT, 17);
        let value = kira_rt_enum_new(4, PAYLOAD_ENUM, cell as u64);
        let copy = kira_rt_enum_clone(value);
        let read = kira_rt_enum_payload(value) as crate::cells::KCell;
        assert_eq!(read, cell);
        kira_rt_enum_free(value);
        kira_rt_enum_free(copy);
        crate::cells::kira_rt_cell_free(read);
    }
    assert_eq!(
        crate::accounting::kira_rt_heap_live(),
        before,
        "the enum and its cell payload have no residual hold"
    );
}

/// An unrecognized kind owns nothing rather than reinterpreting the word.
#[test]
fn an_unknown_payload_kind_is_treated_as_inert() {
    // SAFETY: the handle is live and freed exactly once; the payload word
    // is never dereferenced because the kind is not one that owns.
    unsafe {
        let value = kira_rt_enum_new(0, 99, 0xdead_beef);
        assert_eq!(kira_rt_enum_payload(value), 0xdead_beef);
        let copy = kira_rt_enum_clone(value);
        kira_rt_enum_free(value);
        kira_rt_enum_free(copy);
    }
}

#[test]
fn a_null_handle_is_the_zero_value() {
    // SAFETY: a null handle is a valid tag-0 value; free is a no-op.
    unsafe {
        let empty: KEnum = std::ptr::null_mut();
        assert_eq!(kira_rt_enum_tag(empty), 0);
        assert!(kira_rt_enum_clone(empty).is_null());
        kira_rt_enum_free(empty);
    }
}

/// A payload-less variant is the handle, so constructing one allocates
/// nothing and reading it back costs a shift.
#[test]
fn a_payload_less_variant_lives_in_its_handle() {
    for tag in [0_i64, 1, 7, 1024] {
        let value = inline_handle(tag);
        assert!(is_inline(value));
        // SAFETY: an inline handle is not a pointer and is never read as one.
        assert_eq!(unsafe { kira_rt_enum_tag(value) }, tag);
        // SAFETY: same handle; an inline one has no payload.
        assert_eq!(unsafe { kira_rt_enum_payload(value) }, 0);
    }
}

/// Copying one is identity and releasing one is nothing, which is what
/// makes it free to read in a loop.
#[test]
fn an_inline_variant_owns_nothing() {
    let value = inline_handle(3);
    // SAFETY: an inline handle owns no allocation.
    let copy = unsafe { kira_rt_enum_clone(value) };
    assert_eq!(copy, value);
    // SAFETY: releasing an inline handle reclaims nothing, twice over.
    unsafe {
        kira_rt_enum_free(value);
        kira_rt_enum_free(copy);
    }
    // SAFETY: still readable, because nothing was ever freed.
    assert_eq!(unsafe { kira_rt_enum_tag(value) }, 3);
}

/// A boxed enum comes from the allocator word-aligned, so it never looks
/// inline — the bit that tells them apart is only ever set deliberately.
#[test]
fn a_boxed_enum_is_never_mistaken_for_an_inline_one() {
    let boxed = kira_rt_enum_new(9, PAYLOAD_INERT, 42);
    assert!(!is_inline(boxed));
    // SAFETY: the handle is live.
    assert_eq!(unsafe { kira_rt_enum_tag(boxed) }, 9);
    // SAFETY: the handle is live and freed exactly once.
    unsafe { kira_rt_enum_free(boxed) };
}
