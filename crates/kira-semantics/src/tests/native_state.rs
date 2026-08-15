use super::{codes, diagnostics};

const STATE: &str = r#"
struct CounterState { var count: Int }
@Main function main() {
    var state = nativeState(CounterState { count: 0 })
    var token = nativeUserData(state)
    var view = nativeRecover<CounterState>(token)
    view.count = view.count + 1
    nativeStateFree(state)
}
"#;

#[test]
fn callback_state_intrinsics_type_check_as_first_class_expressions() {
    assert!(diagnostics(STATE).is_empty());
}

#[test]
fn callback_state_intrinsics_check_arity_and_type_arguments() {
    assert_eq!(
        codes("@Main function main() { nativeState(); return }"),
        vec!["KSEM220"]
    );
    assert_eq!(
        codes("@Main function main() { var x = nativeRecover(0); return }"),
        vec!["KSEM216", "KSEM217"]
    );
    assert_eq!(
        codes("@Main function main() { nativeStateFree(1); return }"),
        vec!["KSEM219"]
    );
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
    nativeStateFree(boxed)
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
    nativeStateFree(direct)
    nativeStateFree(nested)
    var total = 0
    let bump: () -> Void = { in total = total + 1 }
    var handler = nativeState(State { payload: .Handler(bump) })
    nativeStateFree(handler)
    nativeStateFree(source)
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
    nativeStateFree(boxed)
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
    nativeStateFree(state)
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
@FFI.Struct { layout: c; }
struct CState { var count: Int }
@Main function main() { var state = nativeState(CState { count: 0 }); return }
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
