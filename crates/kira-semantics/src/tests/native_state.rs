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

// --- KSEM116: a state box the body allocates and then loses -----------------
//
// `nativeState` is the one allocation with no automatic release, so a handle
// that is neither freed nor handed on is a box nothing can name again. These
// pin both halves: that the mistake is reported, and — the half that decides
// whether the check is usable at all — that the idioms which *do* account for
// a handle stay silent.

#[test]
fn a_state_box_that_is_never_freed_is_reported() {
    let text = r#"
struct CounterState { var count: Int }
@Main function main() {
    let state = nativeState(CounterState { count: 0 })
    return
}
"#;
    assert_eq!(codes(text), vec!["KSEM116"]);
}

#[test]
fn freeing_the_handle_accounts_for_it() {
    let text = r#"
struct CounterState { var count: Int }
@Main function main() {
    let state = nativeState(CounterState { count: 0 })
    nativeStateFree(state)
    return
}
"#;
    assert_eq!(codes(text), Vec::<String>::new());
}

/// Handing the token out is the other way to account for a handle: some other
/// owner has it now, and this body is not the one that must free it.
///
/// This is the shape a factory has — `makeRenderEncoder` in Kira Graphics binds
/// a handle, puts its token in the struct it returns, and correctly never frees
/// it — and a check that reported it would be wrong on working code.
#[test]
fn handing_the_token_out_accounts_for_it() {
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
    assert_eq!(codes(text), Vec::<String>::new());
}

/// The documented idiom hands a token out and then frees the handle anyway, so
/// handing out must not consume the binding.
#[test]
fn a_handle_may_be_handed_out_and_still_freed_here() {
    assert!(diagnostics(STATE).is_empty());
}

/// Freeing consumes the handle, so reading it afterwards is a use-after-free —
/// and it is the ordinary use-after-move check that says so.
#[test]
fn reading_a_handle_after_freeing_it_is_use_after_move() {
    let text = r#"
struct CounterState { var count: Int }
@Main function main() {
    let state = nativeState(CounterState { count: 0 })
    nativeStateFree(state)
    let token = nativeUserData(state)
    return
}
"#;
    assert_eq!(codes(text), vec!["KSEM107"]);
}

/// Freeing twice is the same mistake seen once more.
#[test]
fn freeing_a_handle_twice_is_reported() {
    let text = r#"
struct CounterState { var count: Int }
@Main function main() {
    let state = nativeState(CounterState { count: 0 })
    nativeStateFree(state)
    nativeStateFree(state)
    return
}
"#;
    assert_eq!(codes(text), vec!["KSEM107"]);
}

/// A handle cannot be *declared* as a parameter type — `NativeState<T>` is a
/// type the checker infers and prints, not one the grammar parses — so a
/// borrowed handle is unreachable from source today and the `Owned`-only filter
/// in [`FnCtx::unfreed_native_state_handles`] has nothing to exclude yet. It is
/// written that way regardless, because the day the type becomes spellable a
/// `borrow` parameter naming somebody else's box must not be this body's to
/// free, and finding that out then would mean finding it out through a false
/// positive on working code.
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

/// Overwriting a live handle throws away the only token that named the box.
#[test]
fn overwriting_a_live_handle_is_reported() {
    let text = r#"
struct CounterState { var count: Int }
@Main function main() {
    var slot = nativeState(CounterState { count: 1 })
    slot = nativeState(CounterState { count: 2 })
    nativeStateFree(slot)
    return
}
"#;
    assert_eq!(codes(text), vec!["KSEM117"]);
}

/// Free first, then replace. This is the shape a render pass uses to swap its
/// encoder every pass, and reporting it would be reporting correct code.
#[test]
fn freeing_before_replacing_a_handle_is_quiet() {
    let text = r#"
struct CounterState { var count: Int }
@Main function main() {
    var slot = nativeState(CounterState { count: 1 })
    nativeStateFree(slot)
    slot = nativeState(CounterState { count: 2 })
    nativeStateFree(slot)
    return
}
"#;
    assert_eq!(codes(text), Vec::<String>::new());
}

/// Handed out, then replaced: the first box has another owner, so replacing the
/// binding loses nothing.
#[test]
fn replacing_a_handle_that_was_handed_out_is_quiet() {
    let text = r#"
struct CounterState { var count: Int }
function build() -> RawPtr {
    var slot = nativeState(CounterState { count: 1 })
    let first = nativeUserData(slot)
    slot = nativeState(CounterState { count: 2 })
    nativeStateFree(slot)
    return first
}
@Main function main() {
    let p = build()
    return
}
"#;
    assert_eq!(codes(text), Vec::<String>::new());
}

/// A handle freed on only one arm is NOT reported.
///
/// The branch join takes a value moved on either path as moved, which is the
/// sound direction for use-after-move and the unsound one for must-free. The
/// choice is deliberate: this check is worth having only while it never fires
/// on correct code, and a must-free join would need its own state rather than a
/// reinterpretation of that column. Pinned here so the gap is a decision on the
/// record rather than something to rediscover.
#[test]
fn a_handle_freed_on_one_arm_only_is_not_reported_yet() {
    let text = r#"
struct CounterState { var count: Int }
function conditional(flag: Bool) {
    let maybe = nativeState(CounterState { count: 3 })
    if flag {
        nativeStateFree(maybe)
    }
    return
}
@Main function main() {
    conditional(true)
    return
}
"#;
    assert_eq!(codes(text), Vec::<String>::new());
}
