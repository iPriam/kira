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

/// Callback state that holds a **function value** boxes, recovers, and is still
/// callable on every backend.
///
/// This is the shape an application's runtime state actually has: a struct of
/// counters plus the handlers the host calls back into. A function value is a
/// tag and its captures, every one of which had to be trivially copyable to
/// exist, so it boxes as an ordinary struct — and calling the recovered handler
/// is what proves the tag survived the round trip rather than merely the bytes.
#[test]
fn callback_state_holding_a_function_value_round_trips() {
    let output = assert_parity(
        r#"
struct Frame {
    var n: Int
}

function bump(frame: borrow mut Frame) {
    frame.n = frame.n + 1
    return
}

function scale(frame: borrow mut Frame) {
    frame.n = frame.n * 3
    return
}

struct AppState {
    var count: Int
    var onFrame: (borrow mut Frame) -> Void
}

@Main
function main() {
    let boxed = nativeState(AppState { count: 4, onFrame: bump })
    var recovered = nativeRecover<AppState>(nativeUserData(boxed))
    var f = Frame { n: 10 }
    recovered.onFrame(f)
    print(recovered.count)
    print(f.n)

    let other = nativeState(AppState { count: 9, onFrame: scale })
    var second = nativeRecover<AppState>(nativeUserData(other))
    second.onFrame(f)
    print(second.count)
    print(f.n)

    nativeStateFree(boxed)
    nativeStateFree(other)
    return
}
"#,
    );
    // 10 bumped to 11, then scaled to 33 — each state reached its own handler.
    assert_eq!(output, "4\n11\n9\n33\n");
}
