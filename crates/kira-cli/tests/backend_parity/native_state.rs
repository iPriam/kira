use super::assert_parity;

#[test]
fn callback_state_mutation_crosses_runtime_and_native_byte_identically() {
    let output = assert_parity(
        r#"
struct CounterState {
    var count: Int
    var total: Int
}

@Native
function onValue(value: Int, user_data: RawPtr) -> Int {
    var state = nativeRecover<CounterState>(user_data)
    state.count = state.count + 1
    state.total = state.total + value
    return value + state.count
}

@Runtime
function invokeLikeCallback(user_data: RawPtr, value: Int) -> Int {
    return onValue(value, user_data)
}

@Main
@Runtime
function main() {
    var state = nativeState(CounterState { count: 0 total: 0 })
    var token = nativeUserData(state)
    print(invokeLikeCallback(token, 5))
    print(invokeLikeCallback(token, 7))
    var recovered = nativeRecover<CounterState>(token)
    print(recovered.count)
    print(recovered.total)
    nativeStateFree(state)
}
"#,
    );
    assert_eq!(output, "6\n9\n2\n12\n");
}

#[test]
fn native_state_copies_instead_of_consuming_or_aliasing_its_source() {
    let output = assert_parity(
        r#"
struct State { var count: Int }
@Main function main() {
    var original = State { count: 3 }
    var state = nativeState(original)
    original.count = 9
    var recovered = nativeRecover<State>(nativeUserData(state))
    print(original.count)
    print(recovered.count)
    nativeStateFree(state)
}
"#,
    );
    assert_eq!(output, "9\n3\n");
}

#[test]
fn callback_state_carries_raw_pointer_fields() {
    let output = assert_parity(
        r#"
struct State {
    var ctx: RawPtr
    var count: Int
}
function makePtr() -> RawPtr {
    return nativeUserData(nativeState(0))
}
@Main function main() {
    var probe = makePtr()
    var state = nativeState(State { ctx: probe, count: 0 })
    var view = nativeRecover<State>(nativeUserData(state))
    view.count = view.count + 5
    var again = nativeRecover<State>(nativeUserData(state))
    print(again.count)
    nativeStateFree(state)
}
"#,
    );
    assert_eq!(output, "5\n");
}

#[test]
fn callback_state_preserves_enum_payloads() {
    let output = assert_parity(
        r#"
enum Mode { None Some(Int) }
struct State { var mode: Mode }
function code(mode: Mode) -> Int {
    match mode {
        Some(value) -> return value;
        None -> return 0;
    }
    return 0
}
@Main function main() {
    var state = nativeState(State { mode: .Some(42) })
    var recovered = nativeRecover<State>(nativeUserData(state))
    print(code(recovered.mode))
    nativeStateFree(state)
}
"#,
    );
    assert_eq!(output, "42\n");
}

#[test]
fn callback_state_deep_copies_nested_arrays_and_enums() {
    let output = assert_parity(
        r#"
enum Mode { None Surface }
struct Layer { let payload: Mode }
struct State { var layers: [Layer] }

function code(mode: Mode) -> Int {
    match mode {
        Surface -> return 1;
        None -> return 0;
    }
    return 0
}

@Main
function main() {
    var state = nativeState(State { layers: [Layer { payload: .Surface }] })
    var recovered = nativeRecover<State>(nativeUserData(state))
    print(code(recovered.layers[0].payload))
    nativeStateFree(state)
}
"#,
    );
    assert_eq!(output, "1\n");
}
