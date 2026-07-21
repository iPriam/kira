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

#[test]
fn callback_state_rejects_non_owned_and_statically_wrong_types() {
    assert_eq!(
        codes(
            r#"
@FFI.Struct { layout: c; }
struct CState { var count: I64 }
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
