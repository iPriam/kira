use super::{codes, diagnostics};

const STATE: &str = r#"
struct CounterState { var count: Int }
@Main function main() {
    var state = nativeState(CounterState { count: 0 })
    var token = nativeUserData(state)
    var view = nativeRecover<CounterState>(token)
    view.count = view.count + 1
    nativeUserDataRelease(token)
}
"#;

#[test]
fn callback_state_intrinsics_type_check_as_first_class_expressions() {
    assert!(diagnostics(STATE).is_empty());
}

#[test]
fn callback_state_intrinsics_check_arity_and_type_arguments() {
    assert_eq!(
        codes("@Main function main() { nativeState() return }"),
        vec!["KSEM220"]
    );
    assert_eq!(
        codes("@Main function main() { var x = nativeRecover(0) return }"),
        vec!["KSEM216", "KSEM217"]
    );
    assert_eq!(
        codes("@Main function main() { nativeStateFree(1) return }"),
        vec!["KSEM219"]
    );
    assert_eq!(
        codes("@Main function main() { nativeUserDataRetain() return }"),
        vec!["KSEM220"]
    );
    assert_eq!(
        codes("@Main function main() { nativeUserDataRelease(RawPtr(0)) return }"),
        vec!["KSEM379"]
    );
    assert_eq!(
        codes("@Main function main() { nativeUserDataRelease<Int, Int>(RawPtr(0)) return }"),
        vec!["KSEM221"]
    );
}

/// A borrowed raw userdata word never acquires an ownership obligation in Kira.
/// Both manufacturing and destroying an owner through that word are refused.
#[test]
fn raw_userdata_cannot_change_native_state_owner_count() {
    let text = r#"
struct CounterState { var count: Int }
@Main function main() {
    let state = nativeState(CounterState { count: 0 })
    let token = nativeUserDataBorrow(state)
    nativeUserDataRetain(token)
    nativeUserDataRelease(token)
    return
}
"#;
    assert_eq!(codes(text), vec!["KSEM378", "KSEM379"]);
}

/// State may hold a closure that captured a `var`.
///
/// This is what an application's runtime state *is* — a frame handler stored
/// beside the values it reads — and the capture cell inside it goes into the
/// box as a share rather than a copy, so the declaring frame and the boxed
/// closure keep writing through one box.
#[test]
fn callback_state_accepts_a_closure_that_captured_a_var() {
    let text = r#"
struct AppState { var label: String  var bump: () -> Void }
@Main function main() {
    var total = 0
    let bump: () -> Void = { in total = total + 1 }
    let boxed = nativeState(AppState { label: "frames", bump: bump })
    var state = nativeRecover<AppState>(boxed)
    state.bump()
    print(total)
    return
}
"#;
    assert_eq!(codes(text), Vec::<String>::new());
}

/// An owned userdata handle cannot be hidden in a `RawPtr` enum payload. The
/// conversion would erase the owner and permit copies with no matching release.
#[test]
fn callback_state_refuses_owned_tokens_in_raw_pointer_payloads() {
    let text = r#"
enum Inner { Pointer(RawPtr) }
enum Payload {
    Direct(RawPtr)
    Nested(Inner)
    Handler(() -> Void)
}
struct State { var payload: Payload }
@Main function main() {
    var source = nativeState(0)
    let pointer = nativeUserData(source)
    var direct = nativeState(State { payload: .Direct(pointer) })
    var nested = nativeState(State { payload: .Nested(.Pointer(pointer)) })
    var total = 0
    let bump: () -> Void = { in total = total + 1 }
    var handler = nativeState(State { payload: .Handler(bump) })
    return
}
"#;
    assert_eq!(codes(text), vec!["KSEM123", "KSEM123"]);
}

/// What a cell *holds* still answers the eligibility question on its own terms:
/// a captured `var` of a type with no boxed form is refused with everything
/// else that has none.
#[test]
fn a_capture_cell_holding_an_unboxable_value_is_still_refused() {
    let text = r#"
struct Holder { var value: Any  var read: () -> Void }
@Main function main() {
    var erased: Any = 1
    let read: () -> Void = { in print(erased) }
    let boxed = nativeState(Holder { value: erased, read: read })
    return
}
"#;
    assert!(
        codes(text).iter().any(|code| code == "KSEM214"),
        "{:?}",
        codes(text)
    );
}

#[test]
fn callback_state_still_rejects_an_enum_with_an_erased_payload() {
    let text = r#"
enum Payload { Erased(Any) }
struct State { var payload: Payload }
@Main function main() {
    var state = nativeState(State { payload: .Erased(1) })
    return
}
"#;
    assert_eq!(codes(text), vec!["KSEM214"]);
}

#[test]
fn callback_state_rejects_non_owned_and_statically_wrong_types() {
    assert_eq!(
        codes(
            r#"
@FFI.Struct { layout: c }
struct CState { var count: Int }
@Main function main() { var state = nativeState(CState { count: 0 }) return }
"#,
        ),
        vec!["KSEM214"]
    );
    assert_eq!(
        codes(
            r#"
struct Left { var value: Int }
struct Right { var value: Int }
@Main function main() {
    var state = nativeState(Left { value: 0 })
    var wrong = nativeRecover<Right>(state)
    return
}
"#,
        ),
        vec!["KSEM218"]
    );
}

// --- Reference counting: handles, tokens, and the deprecated free -----------
//
// A handle owns one reference and gives it up when it goes out of scope, so
// nothing about a handle's lifetime is reported at compile time any more. What
// the checker still says: `nativeStateFree` is deprecated and consumes the
// handle, and the owner-count intrinsics take tokens.

/// A handle that is never mentioned again releases its reference with its
/// scope, which is the ordinary shape and reports nothing.
#[test]
fn a_handle_releases_its_reference_when_its_scope_ends() {
    let text = r#"
struct CounterState { var count: Int }
@Main function main() {
    let state = nativeState(CounterState { count: 0 })
    return
}
"#;
    assert_eq!(codes(text), Vec::<String>::new());
}

/// `nativeStateFree` still compiles, as one release, and is warned about.
#[test]
fn native_state_free_is_a_deprecated_release() {
    let text = r#"
struct CounterState { var count: Int }
@Main function main() {
    let state = nativeState(CounterState { count: 0 })
    nativeStateFree(state)
    return
}
"#;
    assert_eq!(codes(text), vec!["KSEM360"]);
    let diagnostics = diagnostics(text);
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0].severity, kira_diagnostics::Severity::Warning);
}

/// An owned userdata handle cannot be erased into a `RawPtr` field. Doing so
/// would make its release an unchecked convention rather than a compiler fact.
#[test]
fn an_owned_token_cannot_escape_as_a_raw_pointer() {
    let text = r#"
struct CounterState { var count: Int }
struct Holder { let storage: RawPtr }
function make() -> Holder {
    let state = nativeState(CounterState { count: 0 })
    return Holder { storage: nativeUserData(state) }
}
@Main function main() {
    let held = make()
    return
}
"#;
    assert_eq!(codes(text), vec!["KSEM094"]);
}

/// Exporting a token leaves the handle usable: the export is a read, not a move.
#[test]
fn a_handle_may_be_exported_and_still_used() {
    assert!(diagnostics(STATE).is_empty());
}

/// Releasing through the handle consumes it, so reading it afterwards is the
/// ordinary use after move.
#[test]
fn reading_a_handle_after_releasing_it_is_use_after_move() {
    let text = r#"
struct CounterState { var count: Int }
@Main function main() {
    let state = nativeState(CounterState { count: 0 })
    nativeStateFree(state)
    let token = nativeUserData(state)
    return
}
"#;
    assert_eq!(codes(text), vec!["KSEM360", "KSEM107"]);
}

/// An owner type is writable so state can live in aggregate fields without
/// becoming a copyable `RawPtr`.
#[test]
fn a_handle_type_can_be_declared() {
    let text = r#"
struct CounterState { var count: Int }
function peek(state: NativeState<CounterState>) {
    return
}
@Main function main() {
    return
}
"#;
    assert_eq!(codes(text), Vec::<String>::new());
}

/// Overwriting a live handle releases the reference it held, as assigning over
/// any owned value does, so nothing is reported.
#[test]
fn overwriting_a_live_handle_releases_the_old_one() {
    let text = r#"
struct CounterState { var count: Int }
@Main function main() {
    var slot = nativeState(CounterState { count: 1 })
    slot = nativeState(CounterState { count: 2 })
    return
}
"#;
    assert_eq!(codes(text), Vec::<String>::new());
}

/// Moving a handle transfers its owner, and the moved-from binding is done.
#[test]
fn moving_a_handle_transfers_its_owner() {
    let text = r#"
struct CounterState { var count: Int }
function keep(state: NativeState<CounterState>) {
    return
}
@Main function main() {
    let state = nativeState(CounterState { count: 1 })
    let other = move state
    let token = nativeUserData(state)
    return
}
"#;
    assert_eq!(codes(text), vec!["KSEM107"]);
}

#[test]
fn borrowed_userdata_requires_local_rooted_owner_storage() {
    let text = r#"
@Main function main() {
    let token = nativeUserDataBorrow(nativeState(1))
    return
}
"#;
    assert_eq!(codes(text), vec!["KSEM373"]);
}

#[test]
fn release_has_no_raw_token_type_argument_escape_hatch() {
    let text = r#"
@Main function main() {
    let owner = nativeState(1)
    nativeUserDataRelease<Int>(owner)
    return
}
"#;
    assert_eq!(codes(text), vec!["KSEM221"]);
}

#[test]
fn recover_refuses_an_owned_state_temporary() {
    let text = r#"
@Main function main() {
    var view = nativeRecover<Int>(nativeState(1))
    return
}
"#;
    assert_eq!(codes(text), vec!["KSEM375"]);
}

#[test]
fn affine_state_field_cannot_be_copied_out_of_its_owner() {
    let text = r#"
struct Holder { let state: NativeState<Int> }
@Main function main() {
    let holder = Holder { state: nativeState(1) }
    let second = holder.state
    return
}
"#;
    assert_eq!(codes(text), vec!["KSEM376"]);
}

#[test]
fn a_native_state_owner_is_not_a_root_state_payload() {
    let text = r#"
@Main function main() {
    let inner = nativeState(1)
    let outer = nativeState(move inner)
    return
}
"#;
    assert_eq!(codes(text), vec!["KSEM377"]);
}

#[test]
fn retain_refuses_a_tracked_owner() {
    let text = r#"
@Main function main() {
    let owner = nativeState(1)
    nativeUserDataRetain(owner)
    return
}
"#;
    assert_eq!(codes(text), vec!["KSEM378"]);
}
