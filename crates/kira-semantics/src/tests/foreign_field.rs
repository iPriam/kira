//! Reading a member through an `@FFI.Pointer`.

use super::library_codes;

/// A pointer whose target is a C-layout struct reads that struct's members.
#[test]
fn a_member_reads_through_a_pointer_to_a_c_layout_struct() {
    let source = r#"
@FFI.Struct { layout: c }
struct event {
    let kind: I32 = 0
    let weight: F32 = 0.0
}

@FFI.Pointer { target: event, ownership: borrowed }
struct event_ptr {}

function readKind(e: event_ptr) -> Int {
    return e.kind
}
"#;
    assert!(
        library_codes(source).is_empty(),
        "{:?}",
        library_codes(source)
    );
}

/// A member the target does not have is refused by name.
#[test]
fn a_member_the_target_lacks_is_refused() {
    let source = r#"
@FFI.Struct { layout: c }
struct event {
    let kind: I32 = 0
}

@FFI.Pointer { target: event, ownership: borrowed }
struct event_ptr {}

function readMissing(e: event_ptr) -> Int {
    return e.missing
}
"#;
    assert!(
        library_codes(source).iter().any(|code| code == "KSEM090"),
        "{:?}",
        library_codes(source)
    );
}

/// A nested aggregate member gives its address, so the read chains.
///
/// Its bytes live inside the container, so there is nothing to copy out: the
/// member names a place, and a pointer to that place is what reading it gives.
#[test]
fn a_nested_aggregate_member_gives_a_pointer_into_the_container() {
    let source = r#"
@FFI.Struct { layout: c }
struct point {
    let x: I32 = 0
    let y: I32 = 0
}

@FFI.Struct { layout: c }
struct event {
    let at: point = point {}
}

@FFI.Pointer { target: event, ownership: borrowed }
struct event_ptr {}

function readX(e: event_ptr) -> Int {
    return e.at.x
}
"#;
    assert!(
        library_codes(source).is_empty(),
        "{:?}",
        library_codes(source)
    );
}

/// An inline array member decays to a pointer to its first element, and
/// indexing walks from there — the same meaning C gives both.
#[test]
fn an_inline_array_member_indexes_like_a_c_array() {
    let source = r#"
@FFI.Struct { layout: c }
struct touch {
    let pos_x: F32 = 0.0
}

@FFI.Array { element: touch, count: 4 }
struct touch_array_4 {}

@FFI.Struct { layout: c }
struct event {
    let touches: touch_array_4
}

@FFI.Pointer { target: event, ownership: borrowed }
struct event_ptr {}

function readTouch(e: event_ptr, index: Int) -> Float {
    return e.touches[index].pos_x
}
"#;
    assert!(
        library_codes(source).is_empty(),
        "{:?}",
        library_codes(source)
    );
}

/// A pointer to something that is not a C-layout struct stays an opaque handle.
///
/// Generated bindings point at C types nobody declared and at themselves, so
/// this has to keep working rather than become a diagnostic — and the member
/// read is what is refused, not the declaration.
#[test]
fn a_pointer_to_an_undeclared_target_is_still_an_opaque_handle() {
    let source = r#"
@FFI.Pointer { target: SECURITY_ATTRIBUTES, ownership: borrowed }
struct security_ptr {}

@FFI.Extern { library: fixture, symbol: take, abi: c }
function take(p: security_ptr): Int

function pass(p: security_ptr) -> Int {
    return take(p)
}
"#;
    assert!(
        library_codes(source).is_empty(),
        "{:?}",
        library_codes(source)
    );
}

/// A pointer parameter still accepts the struct itself, handed over by address.
///
/// The pointer carrying its target must not cost the crossing that was already
/// there.
#[test]
fn a_pointer_parameter_still_accepts_the_struct_by_address() {
    let source = r#"
@FFI.Struct { layout: c }
struct desc {
    let tag: I32 = 0
}

@FFI.Pointer { target: desc, ownership: borrowed }
struct desc_ptr {}

@FFI.Extern { library: fixture, symbol: use_desc, abi: c }
function useDesc(d: desc_ptr): Int

function send() -> Int {
    let d = desc { tag: 7 }
    return useDesc(move d)
}
"#;
    assert!(
        library_codes(source).is_empty(),
        "{:?}",
        library_codes(source)
    );
}

/// A pointer word passes as a pointer word, whichever spelling names it.
///
/// A C API hands the same address back as `void*` in one function and as `T*` in
/// the next, and binding files declare both. Knowing the target buys field
/// reads; it does not make the two different values.
#[test]
fn a_typed_pointer_is_accepted_where_a_raw_pointer_is_expected() {
    let source = r#"
@FFI.Struct { layout: c }
struct event {
    let kind: I32 = 0
}

@FFI.Pointer { target: event, ownership: borrowed }
struct event_ptr {}

@FFI.Extern { library: fixture, symbol: event_kind, abi: c }
function eventKind(e: RawPtr): I32

function askC(e: event_ptr) -> Int {
    let word: RawPtr = e
    let back: event_ptr = word
    return eventKind(e) + eventKind(word) + back.kind
}
"#;
    assert!(
        library_codes(source).is_empty(),
        "{:?}",
        library_codes(source)
    );
}
