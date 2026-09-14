use super::*;

/// The marker this archive defines must be the one generated code
/// references, or the guard silently guards nothing: the backend would emit
/// a reference to a symbol no archive ever defines (every link fails), or
/// this archive would define a marker nobody checks (stale archives link
/// again). Bumping `RUNTIME_ABI_VERSION` means renaming the function above.
#[test]
fn the_abi_marker_matches_the_shared_contract() {
    assert_eq!(
        kira_runtime_abi::RUNTIME_ABI_MARKER,
        "kira_rt_abi_version_18"
    );
    assert_eq!(kira_runtime_abi::RUNTIME_ABI_VERSION, 18);
    // Referenced so the marker cannot be dead-code-eliminated out of an
    // rlib build, and so a rename breaks this test rather than the link.
    kira_rt_abi_version_18();
}

/// The backend reads `shares` out of this object, so its shape is a
/// contract with code compiled separately from this crate. The `Box<[u8]>`
/// in front of it is a fat pointer — two words — which is what puts the
/// count at field index two.
#[test]
fn the_string_layout_is_pinned() {
    assert_eq!(size_of::<Box<[u8]>>(), 2 * size_of::<usize>());
    assert_eq!(size_of::<KiraString>(), 3 * size_of::<usize>());
    assert_eq!(align_of::<KiraString>(), align_of::<usize>());
    let owned = KiraString {
        bytes: Box::new([]),
        shares: 1,
    };
    let base = std::ptr::from_ref(&owned).cast::<u8>();
    // SAFETY: both fields belong to `owned`, which outlives the reads.
    unsafe {
        assert_eq!(
            std::ptr::from_ref(&owned.bytes)
                .cast::<u8>()
                .offset_from(base),
            0
        );
        assert_eq!(
            std::ptr::from_ref(&owned.shares)
                .cast::<u8>()
                .offset_from(base),
            isize::try_from(kira_runtime_abi::STRING_SHARES_FIELD).expect("a small index")
                * size_of::<usize>() as isize
        );
    }
}

/// Builds a handle from a literal, as the backend's lowering would.
fn new(text: &str) -> KStr {
    // SAFETY: the slice covers exactly `len` readable bytes.
    unsafe { kira_rt_str_new(text.as_ptr(), text.len()) }
}

#[test]
fn concat_clone_and_eq_follow_value_semantics() {
    // SAFETY: every handle below is live and consumed exactly once.
    unsafe {
        let joined = kira_rt_str_concat(kira_rt_str_concat(new("hello"), new(", ")), new("world"));
        assert_eq!(bytes_of(joined), b"hello, world");

        let copy = kira_rt_str_clone(joined);
        assert_eq!(bytes_of(copy), bytes_of(joined));
        // A copy is the same object, held twice: the bytes are never
        // written, so nothing can tell the two apart.
        assert_eq!(copy, joined, "a copy allocates nothing");
        assert_eq!((*joined).shares, 2);

        assert_eq!(kira_rt_str_eq(joined, copy), 1); // releases both
    }
}

/// The bytes go with the last value holding them, never with the first —
/// which under Miri or ASan is the difference between a live read and a
/// use-after-free.
#[test]
fn a_shared_string_outlives_every_hold_but_the_last() {
    // SAFETY: the handle is live and released once per hold.
    unsafe {
        let text = new("payload");
        let copy = kira_rt_str_clone(text);
        kira_rt_str_free(text);
        assert_eq!(bytes_of(copy), b"payload", "the bytes survived one hold");
        kira_rt_str_free(copy);
    }
}

#[test]
fn distinct_contents_compare_unequal() {
    // SAFETY: both handles are live and consumed by the comparison.
    unsafe {
        assert_eq!(kira_rt_str_eq(new("hello"), new("kira")), 0);
    }
}

/// The host's only way to read a handle: it cannot dereference `KiraString`,
/// which is this crate's private type.
#[test]
fn a_handles_bytes_are_readable_from_outside_this_crate() {
    // SAFETY: `handle` is live for both reads and freed exactly once.
    unsafe {
        let handle = new("hello, world");
        let data = kira_rt_str_data(handle);
        let len = kira_rt_str_len(handle);
        assert_eq!(slice::from_raw_parts(data, len), b"hello, world");
        kira_rt_str_free(handle);
    }
}

#[test]
fn the_null_handle_is_the_empty_string() {
    // SAFETY: a null handle is a valid empty string; free is a no-op.
    unsafe {
        let empty: KStr = std::ptr::null_mut();
        assert_eq!(bytes_of(empty), b"");
        assert!(kira_rt_str_clone(empty).is_null());
        assert_eq!(kira_rt_str_eq(empty, new("")), 1);
        kira_rt_str_free(empty);
    }
}
