//! Tests for the running instance: calls by name, the host it owns, argument
//! checking, and the balance an instance owes when it ends.

use super::*;
use crate::error::Error;
use crate::fixture::artifact;
use crate::library::Library;
use kira_runtime_abi::CapturingHost;
use kira_vm_runtime::VmError;

fn library() -> Library {
    Library::from_bytes(&artifact()).expect("the fixture is a valid library")
}

fn capturing() -> Instance<CapturingHost> {
    library()
        .instantiate_with(CapturingHost::new())
        .expect("it loads")
}

/// The shape of the motivating consumer, end to end on this surface: make an
/// object, ask about it in a later call, re-enter with an event, let it go.
#[test]
fn an_object_made_by_one_call_answers_a_later_one() {
    let mut ui = capturing();

    let made = ui
        .call("make_button", &[NativeArg::Str("ok")])
        .expect("clean call");
    let NativeResult::Handle(word) = made else {
        panic!("a class result crosses as a handle, got {made:?}")
    };
    let button = Handle::from_word(word);
    assert_eq!(ui.live_handles(), 1);

    let label = ui
        .call("button_label", &[NativeArg::Handle(button.as_word())])
        .expect("clean call");
    assert_eq!(label, NativeResult::Str("ok".to_owned()));

    // Rust re-entering Kira per event is a plain call, and needs no machinery
    // beyond the one that made the object.
    let hit = ui
        .call(
            "click_at",
            &[NativeArg::Handle(button.as_word()), NativeArg::Int(4)],
        )
        .expect("clean call");
    assert_eq!(hit, NativeResult::Bool(true));

    // The call mutated its own copy; the object the handle names is unchanged,
    // which is the same answer Kira gives a Kira caller.
    let again = ui
        .call("button_label", &[NativeArg::Handle(button.as_word())])
        .expect("clean call");
    assert_eq!(again, NativeResult::Str("ok".to_owned()));

    ui.release(button).expect("it was live");
    assert_eq!(ui.live_handles(), 0);
    assert_eq!(ui.finish().current, 0, "the instance balanced");
}

#[test]
fn an_export_this_library_does_not_have_is_named_rather_than_guessed_at() {
    let mut ui = capturing();
    let error = ui
        .call("make_window", &[])
        .expect_err("nothing by that name");
    assert_eq!(
        error,
        Error::UnknownExport {
            name: "make_window".to_owned(),
        }
    );
}

#[test]
fn too_few_arguments_are_refused_with_both_counts() {
    let mut ui = capturing();
    let error = ui
        .call("click_at", &[NativeArg::Int(1)])
        .expect_err("arity");
    assert_eq!(
        error,
        Error::ArgumentCount {
            export: "click_at".to_owned(),
            expected: 2,
            found: 1,
        }
    );
}

#[test]
fn too_many_arguments_are_refused_with_both_counts() {
    let mut ui = capturing();
    let error = ui
        .call(
            "make_button",
            &[NativeArg::Str("ok"), NativeArg::Str("extra")],
        )
        .expect_err("arity");
    assert_eq!(
        error,
        Error::ArgumentCount {
            export: "make_button".to_owned(),
            expected: 1,
            found: 2,
        }
    );
}

/// A mistake entirely outside the library is named outside it. Left to the VM
/// this would be a trap from somewhere inside, which says nothing about the
/// argument that was wrong.
#[test]
fn an_argument_of_the_wrong_kind_is_named_before_the_vm_runs() {
    let mut ui = capturing();
    let error = ui
        .call("make_button", &[NativeArg::Int(7)])
        .expect_err("a string parameter took an integer");
    assert_eq!(
        error,
        Error::ArgumentType {
            export: "make_button".to_owned(),
            position: 0,
            expected: "a string",
            found: "an integer",
        }
    );
}

#[test]
fn the_position_reported_is_the_argument_that_was_wrong() {
    let mut ui = capturing();
    let made = ui
        .call("make_button", &[NativeArg::Str("ok")])
        .expect("clean call");
    let NativeResult::Handle(word) = made else {
        panic!("expected a handle, got {made:?}")
    };
    let error = ui
        .call("click_at", &[NativeArg::Handle(word), NativeArg::Str("4")])
        .expect_err("the second argument is an integer");
    assert_eq!(
        error,
        Error::ArgumentType {
            export: "click_at".to_owned(),
            position: 1,
            expected: "an integer",
            found: "a string",
        }
    );
    ui.release(Handle::from_word(word)).expect("it was live");
    assert_eq!(ui.finish().current, 0, "a refused call allocated nothing");
}

#[test]
fn a_handle_is_not_interchangeable_with_the_integer_it_is_a_word_of() {
    let mut ui = capturing();
    let error = ui
        .call("button_label", &[NativeArg::Int(1)])
        .expect_err("a handle parameter took an integer");
    assert_eq!(
        error,
        Error::ArgumentType {
            export: "button_label".to_owned(),
            position: 0,
            expected: "a handle",
            found: "an integer",
        }
    );
}

/// Class typing is the generated newtypes' job, and this layer says so by
/// letting a wrong-class handle through to the instance, which refuses it as a
/// root it never minted rather than as a type error it cannot see.
#[test]
fn a_handle_from_nowhere_is_a_dangling_root_not_a_type_error() {
    let mut ui = capturing();
    let error = ui
        .call("button_label", &[NativeArg::Handle(4242)])
        .expect_err("no such root");
    assert!(
        matches!(error, Error::Vm(VmError::DanglingRoot { root: 4242 })),
        "got {error:?}"
    );
    assert_eq!(ui.finish().current, 0, "a refused call left nothing behind");
}

#[test]
fn releasing_a_handle_twice_is_a_typed_error_not_a_second_free() {
    let mut ui = capturing();
    let NativeResult::Handle(word) = ui
        .call("make_button", &[NativeArg::Str("ok")])
        .expect("clean call")
    else {
        panic!("expected a handle")
    };
    let button = Handle::from_word(word);
    ui.release(button).expect("the first release frees it");
    let error = ui.release(button).expect_err("the second names nothing");
    assert!(
        matches!(error, Error::Vm(VmError::DanglingRoot { .. })),
        "got {error:?}"
    );
    assert_eq!(ui.finish().current, 0);
}

/// The default host is replaceable, and replacing it is how an embedder sees
/// what a library printed. This is the same `print` the CLI sends to stdout.
#[test]
fn the_host_an_embedder_supplies_receives_what_the_library_prints() {
    let mut ui = capturing();
    let result = ui
        .call("greet", &[NativeArg::Str("world")])
        .expect("clean call");
    assert_eq!(result, NativeResult::Void);
    assert_eq!(ui.host().lines(), ["world".to_owned()]);

    ui.call("greet", &[NativeArg::Str("again")])
        .expect("clean call");
    assert_eq!(ui.into_host().into_output(), "world\nagain\n");
}

/// The default exists so an embedder that has no opinion needs none. It is
/// exercised for real — output goes to the test process's stdout — because a
/// default nothing constructs is a default nothing proves.
#[test]
fn a_library_instantiated_with_no_opinion_gets_the_stdout_host() {
    let mut ui: Instance<StdoutHost> = library().instantiate().expect("it loads");
    assert_eq!(
        ui.call("greet", &[NativeArg::Str("to stdout")])
            .expect("clean call"),
        NativeResult::Void
    );
    assert_eq!(*ui.host(), StdoutHost);
    assert_eq!(ui.finish().current, 0);
}

/// Dropping without releasing is a leak with no later moment at which it shows,
/// so both exits balance the heap on the way out.
#[test]
fn an_instance_ended_with_live_handles_still_balances() {
    let mut ui = capturing();
    for _ in 0..3 {
        ui.call("make_button", &[NativeArg::Str("ok")])
            .expect("clean call");
    }
    assert_eq!(ui.live_handles(), 3);
    assert_eq!(ui.finish().current, 0, "`finish` releases what is left");

    let mut ui = capturing();
    ui.call("make_button", &[NativeArg::Str("ok")])
        .expect("clean call");
    // `into_host` is the other exit, and owes the same balance.
    assert_eq!(ui.into_host().lines(), Vec::<String>::new());
}

#[test]
fn release_all_frees_every_handle_at_once() {
    let mut ui = capturing();
    for _ in 0..4 {
        ui.call("make_button", &[NativeArg::Str("ok")])
            .expect("clean call");
    }
    ui.release_all();
    assert_eq!(ui.live_handles(), 0);
    assert_eq!(ui.finish().current, 0);
}

/// Debug prints what a consumer may observe and nothing of the heap behind it.
#[test]
fn the_debug_rendering_exposes_no_library_internals() {
    let ui = capturing();
    let rendered = format!("{ui:?}");
    assert!(rendered.contains("live_handles: 0"), "{rendered}");
    assert!(rendered.contains("exports: 4"), "{rendered}");
}
