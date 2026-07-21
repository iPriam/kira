//! Tests for the persistent instance: rooting, release, dangling roots, and
//! what "balanced" means once a heap outlives a call.

use super::*;
use kira_bytecode::module::{FuncProto, Module};
use kira_bytecode::op::Instruction as I;
use kira_runtime_abi::CapturingHost;

fn func(name: &str, params: u16, locals: u16, code: Vec<I>) -> FuncProto {
    FuncProto {
        name: name.to_owned(),
        param_count: params,
        local_count: locals,
        execution: kira_runtime_abi::Execution::Runtime,
        code,
    }
}

/// A stand-in for the § 2 library: a one-field "Button" class plus the exports
/// that make one, read it back, and answer a question about it.
///
/// Function ids: 0 `main`, 1 `make_button(title) -> Button`,
/// 2 `button_label(b) -> String`, 3 `click_at(b, x) -> Bool`,
/// 4 `boom()` (traps), 5 `titles() -> [String]`.
fn library() -> Module {
    let make_button = func(
        "make_button",
        1,
        1,
        vec![I::LoadLocal(0), I::NewStruct(1), I::Return],
    );
    let button_label = func(
        "button_label",
        1,
        1,
        vec![I::LoadLocal(0), I::GetField(0), I::Return],
    );
    // Mutates its copy of the button, then answers about the argument it was
    // given — the mutation is deliberately unobservable to the caller.
    let click_at = func(
        "click_at",
        2,
        2,
        vec![
            I::ConstStr(1),
            I::StoreField {
                slot: 0,
                path: kira_bytecode::op::FieldPath::new(vec![0]).expect("a one-step path"),
            },
            I::LoadLocal(1),
            I::ConstInt(0),
            I::GeInt,
            I::Return,
        ],
    );
    let boom = func(
        "boom",
        0,
        0,
        vec![
            I::ConstStr(0),
            I::ConstInt(1),
            I::ConstInt(0),
            I::DivInt,
            I::Return,
        ],
    );
    let titles = func(
        "titles",
        0,
        0,
        vec![I::ConstStr(0), I::NewArray(1), I::Return],
    );
    Module {
        exports: Default::default(),
        foreign_imports: Vec::new(),
        functions: vec![
            func("main", 0, 0, vec![I::ReturnVoid]),
            make_button,
            button_label,
            click_at,
            boom,
            titles,
        ],
        main: Some(0),
        strings: vec!["ok".to_owned(), "clicked".to_owned()],
    }
}

fn instance() -> Instance {
    Instance::load(library()).expect("a valid module")
}

/// The whole point: an object made by one call is still there for the next one.
#[test]
fn an_object_survives_the_call_that_made_it() {
    let mut host = CapturingHost::new();
    let mut ui = instance();

    let made = ui
        .call(&mut host, 1, &[NativeArg::Str("ok")])
        .expect("clean call");
    let NativeResult::Handle(button) = made else {
        panic!("a class result crosses as a handle, got {made:?}");
    };
    assert_eq!(ui.live_roots(), 1);

    // A separate call, on the same instance, reads the object back.
    let label = ui
        .call(&mut host, 2, &[NativeArg::Handle(button)])
        .expect("clean call");
    assert_eq!(label, NativeResult::Str("ok".to_owned()));

    // And again: reading a handle does not consume it.
    let again = ui
        .call(&mut host, 2, &[NativeArg::Handle(button)])
        .expect("clean call");
    assert_eq!(again, NativeResult::Str("ok".to_owned()));

    ui.release(RootId::from_word(button)).expect("a live root");
    assert_eq!(ui.live_roots(), 0);
    assert_eq!(ui.finish().current, 0);
}

/// Balanced, restated for a heap that outlives a call: everything live between
/// calls is owned by a live root, so releasing them all leaves nothing.
#[test]
fn everything_live_between_calls_is_owned_by_a_live_root() {
    let mut host = CapturingHost::new();
    let mut ui = instance();

    let mut handles = Vec::new();
    for _ in 0..4 {
        let made = ui
            .call(&mut host, 1, &[NativeArg::Str("ok")])
            .expect("clean call");
        let NativeResult::Handle(word) = made else {
            panic!("expected a handle, got {made:?}");
        };
        handles.push(word);
    }
    assert_eq!(ui.live_roots(), 4);
    // Four roots hold storage, so the heap is not empty — and that is correct,
    // not a leak.
    assert!(ui.stats().current > 0);

    for word in &handles {
        ui.release(RootId::from_word(*word)).expect("a live root");
    }
    // With no root left, the old rule applies again, before `finish` is called.
    assert_eq!(ui.stats().current, 0);
    assert_eq!(ui.finish().current, 0);
}

/// A call that returns no object leaves nothing behind at all — the instance
/// reduces to the per-call rule when nothing is rooted.
#[test]
fn a_call_that_roots_nothing_balances_immediately() {
    let mut host = CapturingHost::new();
    let mut ui = instance();
    let made = ui
        .call(&mut host, 1, &[NativeArg::Str("ok")])
        .expect("clean call");
    let NativeResult::Handle(button) = made else {
        panic!("expected a handle");
    };
    let before = ui.stats().current;
    let hit = ui
        .call(
            &mut host,
            3,
            &[NativeArg::Handle(button), NativeArg::Int(4)],
        )
        .expect("clean call");
    assert_eq!(hit, NativeResult::Bool(true));
    // The call allocated a copy of the button and a string, and freed both.
    assert_eq!(ui.stats().current, before);
    assert_eq!(ui.finish().current, 0);
}

/// A handle argument is *copied*, which is what a class already means in Kira:
/// the callee's mutation does not reach the caller's object.
#[test]
fn a_handle_argument_is_copied_so_the_call_cannot_mutate_the_root() {
    let mut host = CapturingHost::new();
    let mut ui = instance();
    let NativeResult::Handle(button) = ui
        .call(&mut host, 1, &[NativeArg::Str("ok")])
        .expect("clean call")
    else {
        panic!("expected a handle");
    };

    // `click_at` writes "clicked" over the title of the button it was handed.
    ui.call(
        &mut host,
        3,
        &[NativeArg::Handle(button), NativeArg::Int(1)],
    )
    .expect("clean call");

    let label = ui
        .call(&mut host, 2, &[NativeArg::Handle(button)])
        .expect("clean call");
    assert_eq!(label, NativeResult::Str("ok".to_owned()));
    assert_eq!(ui.finish().current, 0);
}

/// The property that makes a use-after-free unrepresentable: a released root is
/// a typed error forever, never a hit on whatever later took its heap slot.
#[test]
fn a_released_handle_is_a_typed_error_and_never_another_object() {
    let mut host = CapturingHost::new();
    let mut ui = instance();
    let NativeResult::Handle(first) = ui
        .call(&mut host, 1, &[NativeArg::Str("ok")])
        .expect("clean call")
    else {
        panic!("expected a handle");
    };
    ui.release(RootId::from_word(first)).expect("a live root");

    // Allocating again reuses the heap slot the released object had. The root id
    // is not reused, so the stale handle still names nothing.
    let NativeResult::Handle(second) = ui
        .call(&mut host, 1, &[NativeArg::Str("ok")])
        .expect("clean call")
    else {
        panic!("expected a handle");
    };
    assert_ne!(first, second, "a root id is never minted twice");

    assert_eq!(
        ui.call(&mut host, 2, &[NativeArg::Handle(first)]),
        Err(VmError::DanglingRoot { root: first })
    );
    assert_eq!(
        ui.release(RootId::from_word(first)),
        Err(VmError::DanglingRoot { root: first })
    );
    let error = VmError::DanglingRoot { root: first };
    assert!(
        error.to_string().contains("names no live object"),
        "the refusal says what it refused: {error}"
    );

    ui.release(RootId::from_word(second)).expect("a live root");
    assert_eq!(ui.finish().current, 0);
}

/// A word that was never a root — including the zero a zeroed struct carries —
/// is the same typed refusal.
#[test]
fn a_word_that_was_never_a_root_is_refused() {
    let mut host = CapturingHost::new();
    let mut ui = instance();
    for word in [0, 1, 9_999] {
        assert_eq!(
            ui.call(&mut host, 2, &[NativeArg::Handle(word)]),
            Err(VmError::DanglingRoot { root: word }),
        );
    }
    // Nothing was allocated by a refused call.
    assert_eq!(ui.finish().current, 0);
}

/// A refusal on the second argument frees the first: a rejected call leaves the
/// heap exactly as it found it.
#[test]
fn a_call_refused_partway_through_its_arguments_frees_what_it_lowered() {
    let mut host = CapturingHost::new();
    let mut ui = instance();
    let error = ui
        .call(
            &mut host,
            3,
            &[NativeArg::Str("lowered first"), NativeArg::Handle(404)],
        )
        .expect_err("a dead handle");
    assert_eq!(error, VmError::DanglingRoot { root: 404 });
    assert_eq!(ui.stats().current, 0, "the string argument was reclaimed");
    assert_eq!(ui.finish().current, 0);
}

/// A trap must not leak into a heap that outlives the call.
///
/// `boom` allocates a string, then divides by zero with the string still on the
/// operand stack. Before the instance existed, the heap died with the call and
/// the leak was invisible; now it would be permanent, so the trap unwinds.
#[test]
fn a_trap_reclaims_everything_it_had_live() {
    let mut host = CapturingHost::new();
    let mut ui = instance();
    assert_eq!(
        ui.call(&mut host, 4, &[]),
        Err(VmError::DivideByZero),
        "the trap surfaces typed",
    );
    assert_eq!(ui.stats().current, 0, "the trap freed what it had live");
    assert!(ui.stats().allocated > 0, "it really did allocate first");
    assert_eq!(ui.finish().current, 0);
}

/// The instance is usable after a trap: the heap is intact, not poisoned.
#[test]
fn an_instance_still_works_after_a_call_traps() {
    let mut host = CapturingHost::new();
    let mut ui = instance();
    let NativeResult::Handle(button) = ui
        .call(&mut host, 1, &[NativeArg::Str("ok")])
        .expect("clean call")
    else {
        panic!("expected a handle");
    };
    assert_eq!(ui.call(&mut host, 4, &[]), Err(VmError::DivideByZero));
    // The root the trap ran alongside is untouched.
    assert_eq!(
        ui.call(&mut host, 2, &[NativeArg::Handle(button)]),
        Ok(NativeResult::Str("ok".to_owned()))
    );
    ui.release(RootId::from_word(button)).expect("a live root");
    assert_eq!(ui.finish().current, 0);
}

/// An uncrossable result is refused by name rather than turned into some other
/// value — and the value it refused is freed, not stranded.
#[test]
fn a_result_that_cannot_cross_is_refused_by_name() {
    let mut host = CapturingHost::new();
    let mut ui = instance();
    let error = ui
        .call(&mut host, 5, &[])
        .expect_err("an array cannot cross");
    assert_eq!(
        error,
        VmError::UncrossableExport {
            function: 5,
            kind: "an array result",
        }
    );
    assert_eq!(
        error.to_string(),
        "an array result cannot cross the export boundary at function 5"
    );
    assert_eq!(ui.stats().current, 0, "the refused array was freed");
    assert_eq!(ui.finish().current, 0);
}

/// A host driving an instance from an artifact that disagrees with the module is
/// refused exactly as [`Program::call`] refuses it — one rule, two doors.
#[test]
fn a_bad_request_is_refused_the_same_way_through_either_door() {
    let mut host = CapturingHost::new();
    let mut ui = instance();
    assert_eq!(
        ui.call(&mut host, 2, &[]),
        Err(VmError::ArityMismatch {
            function: 2,
            expected: 1,
            got: 0,
        })
    );
    assert_eq!(
        ui.call(&mut host, 99, &[]),
        Err(VmError::UnknownFunction(99))
    );
    assert_eq!(ui.finish().current, 0);
}

/// Dropping an instance without finishing it frees its heap like any other Rust
/// value — `finish` is where the *accounting* happens, not where the memory is
/// released.
#[test]
fn dropping_an_instance_with_live_roots_is_safe() {
    let mut host = CapturingHost::new();
    let mut ui = instance();
    ui.call(&mut host, 1, &[NativeArg::Str("ok")])
        .expect("clean call");
    assert_eq!(ui.live_roots(), 1);
    drop(ui);
}

/// Root ids are minted, never recycled, and never zero.
#[test]
fn root_ids_are_minted_in_order_and_never_zero() {
    let mut ui = instance();
    let first = ui.mint_root().expect("a fresh id");
    let second = ui.mint_root().expect("a fresh id");
    assert_eq!(first, RootId::from_word(1));
    assert_eq!(second, RootId::from_word(2));
    assert_eq!(second.as_word(), 2);

    // Exhaustion converts typed rather than wrapping onto a live root.
    ui.next_root = u64::MAX - 1;
    assert_eq!(ui.mint_root(), Ok(RootId::from_word(u64::MAX - 1)));
    assert_eq!(ui.mint_root(), Err(VmError::RootSpaceExhausted));
    // And it stays exhausted rather than wrapping back onto root 1.
    assert_eq!(ui.mint_root(), Err(VmError::RootSpaceExhausted));
}

/// `release_all` is what `finish` leans on, and is idempotent.
#[test]
fn releasing_everything_twice_is_harmless() {
    let mut host = CapturingHost::new();
    let mut ui = instance();
    ui.call(&mut host, 1, &[NativeArg::Str("ok")])
        .expect("clean call");
    ui.release_all();
    assert_eq!(ui.live_roots(), 0);
    ui.release_all();
    assert_eq!(ui.finish().current, 0);
}

/// Entering a `@Native` function must be refused, not attempted.
#[test]
fn entering_a_native_function_is_refused() {
    let mut module = library();
    let mut native = func("hot", 0, 0, vec![]);
    native.execution = kira_runtime_abi::Execution::Native;
    module.functions.push(native);
    let id = (module.functions.len() - 1) as u32;
    let mut ui = Instance::load(module).expect("a valid module");
    let mut host = CapturingHost::new();
    assert_eq!(
        ui.call(&mut host, id, &[]),
        Err(VmError::NativeEntry { function: id })
    );
}

/// A module that passes structural validation while being ill-typed on the
/// operand stack, so every one of its functions traps somewhere that has a live
/// heap value in hand.
///
/// `Module::validate` proves indices and operands in range, not stack typing —
/// so a `.kbc` from anywhere but this compiler can reach these paths. Each
/// function is return-terminated (validation requires it) and traps before ever
/// reaching that return.
///
/// Function ids: 0 `main`, 1 mismatched second operand of `DivInt`,
/// 2 mismatched second operand of `ConcatStr`, 3 `GetField` past the last field,
/// 4 `EnumPayload` on a payload-less variant, 5 a `Call` whose arguments are not
/// all on the stack.
fn ill_typed() -> Module {
    let bad_div = func(
        "bad_div",
        0,
        0,
        // [Str, Int] — the right operand pops as an Int, the left is a string.
        vec![
            I::ConstStr(0),
            I::ConstInt(1),
            I::DivInt,
            I::ConstVoid,
            I::Return,
        ],
    );
    let bad_concat = func(
        "bad_concat",
        0,
        0,
        // [Int, Str] — the right operand pops as a string, the left is an Int,
        // so the failure happens with a live string already in hand.
        vec![
            I::ConstInt(1),
            I::ConstStr(0),
            I::ConcatStr,
            I::ConstVoid,
            I::Return,
        ],
    );
    let bad_field = func(
        "bad_field",
        0,
        0,
        vec![
            I::ConstStr(0),
            I::NewStruct(1),
            I::GetField(7),
            I::ConstVoid,
            I::Return,
        ],
    );
    let bad_payload = func(
        "bad_payload",
        0,
        0,
        vec![
            I::NewEnum {
                tag: 0,
                has_payload: false,
            },
            I::EnumPayload,
            I::ConstVoid,
            I::Return,
        ],
    );
    // Calls a two-parameter function with only one argument pushed, so the
    // callee frame is half filled when the second pop underflows.
    let bad_call = func(
        "bad_call",
        0,
        0,
        vec![I::ConstStr(0), I::Call(6), I::Return],
    );
    let takes_two = func("takes_two", 2, 2, vec![I::ConstVoid, I::Return]);
    Module {
        exports: Default::default(),
        foreign_imports: Vec::new(),
        functions: vec![
            func("main", 0, 0, vec![I::ReturnVoid]),
            bad_div,
            bad_concat,
            bad_field,
            bad_payload,
            bad_call,
            takes_two,
        ],
        main: Some(0),
        strings: vec!["ok".to_owned()],
    }
}

/// The instance's balance invariant holds for *every* trap a validating module
/// can reach, not only the ones well-typed bytecode produces.
///
/// A heap that dies with its call hides a value stranded on an error path; a
/// heap that outlives the call turns the same stranding into a permanent leak
/// that `finish` would report forever. So each trap below is checked twice: it
/// surfaces typed, and it left nothing behind.
#[test]
fn every_trap_a_validating_module_can_reach_still_balances() {
    let expected = [
        (1, VmError::TypeMismatch { expected: "Int" }),
        (2, VmError::TypeMismatch { expected: "String" }),
        (3, VmError::NoSuchField { index: 7 }),
        (4, VmError::MissingEnumPayload),
        (5, VmError::StackUnderflow),
    ];
    for (function, trap) in expected {
        let mut host = CapturingHost::new();
        let mut ui = Instance::load(ill_typed()).expect("a structurally valid module");
        assert_eq!(
            ui.call(&mut host, function, &[]),
            Err(trap),
            "function {function}"
        );
        assert!(
            ui.stats().allocated > 0,
            "function {function} allocated before it trapped"
        );
        assert_eq!(
            ui.stats().current,
            0,
            "function {function} stranded a value in the persistent heap"
        );
        assert_eq!(ui.finish().current, 0, "function {function}");
    }
}
