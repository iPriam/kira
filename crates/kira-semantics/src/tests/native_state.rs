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
        codes("@Main function main() { nativeUserDataRelease<Int>(RawPtr(0)) return }"),
        vec!["KSEM221"]
    );
}

/// The owner-count intrinsics take the exported token, never the handle: a
/// handle releases its own reference by going out of scope.
#[test]
fn owner_count_intrinsics_take_a_raw_pointer_token() {
    let text = r#"
struct CounterState { var count: Int }
@Main function main() {
    let state = nativeState(CounterState { count: 0 })
    nativeUserDataRetain(state)
    nativeUserDataRelease(state)
    return
}
"#;
    assert_eq!(codes(text), vec!["KSEM361", "KSEM361"]);
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
    var state = nativeRecover<AppState>(nativeUserData(boxed))
    state.bump()
    print(total)
    return
}
"#;
    assert_eq!(codes(text), Vec::<String>::new());
}

/// A callback-state enum admits pointer words directly and through another
/// enum, while a closure payload carries its captured `var` as a shared cell.
#[test]
fn callback_state_admits_pointer_and_captured_cell_enum_payloads() {
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
    assert_eq!(codes(text), Vec::<String>::new());
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
    var wrong = nativeRecover<Right>(nativeUserData(state))
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

/// A token may leave the body that made it: it owns a reference of its own,
/// and whoever holds it releases that one.
#[test]
fn a_token_may_leave_the_body_that_exported_it() {
    let text = r#"
struct CounterState { var count: Int }
struct Holder { let storage: RawPtr }
function make() -> Holder {
    let state = nativeState(CounterState { count: 0 })
    return Holder { storage: nativeUserData(state) }
}
@Main function main() {
    let held = make()
    nativeUserDataRelease(held.storage)
    return
}
"#;
    assert_eq!(codes(text), Vec::<String>::new());
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

/// A handle cannot be *declared* as a parameter type — `NativeState<T>` is a
/// type the checker infers and prints, not one the grammar parses.
#[test]
fn a_handle_type_is_inferred_rather_than_written() {
    let text = r#"
struct CounterState { var count: Int }
function peek(state: NativeState<CounterState>) {
    return
}
@Main function main() {
    return
}
"#;
    assert_eq!(codes(text), vec!["KSEM050"]);
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
    assert_eq!(codes(text), vec!["KSEM050", "KSEM107"]);
}
