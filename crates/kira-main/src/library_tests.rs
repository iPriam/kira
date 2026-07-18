//! Tests for loading a library and checking it is the one you were promised.

use super::*;
use crate::fixture::{BUTTON, artifact, library as fixture_library};
use crate::{ContractError, Error, Handle};
use kira_runtime_abi::{NativeArg, NativeResult};

/// The contract a generator would emit for the fixture: every export, in the
/// wrapper's own spelling, plus the hash of the bytes it was built from.
fn contract(hash: u64) -> ExportContract<'static> {
    ExportContract {
        classes: &["Button"],
        functions: &[
            ExpectedExport {
                name: "make_button",
                params: &[ExportType::String],
                result: BUTTON,
            },
            ExpectedExport {
                name: "button_label",
                params: &[BUTTON],
                result: ExportType::String,
            },
            ExpectedExport {
                name: "click_at",
                params: &[BUTTON, ExportType::Int],
                result: ExportType::Bool,
            },
            ExpectedExport {
                name: "greet",
                params: &[ExportType::String],
                result: ExportType::Void,
            },
        ],
        content_hash: hash,
    }
}

fn loaded() -> Library {
    Library::from_bytes(&artifact()).expect("the fixture is a valid library")
}

#[test]
fn a_library_round_trips_through_its_own_bytes() {
    let library = loaded();
    assert_eq!(library.exports(), &fixture_library().exports);
    assert_eq!(library.module().main, None, "a library has no entrypoint");
    assert_eq!(library.exports().functions.len(), 4);
}

#[test]
fn an_export_resolves_by_the_name_a_consumer_knows() {
    let library = loaded();
    let export = library.export("make_button").expect("it is exported");
    assert_eq!(export.function, 0);
    assert_eq!(export.result, BUTTON);
    assert!(
        library.export("makeButton").is_none(),
        "the Kira spelling is not the consumer's"
    );
}

#[test]
fn a_library_verifies_against_the_contract_it_was_generated_from() {
    let library = loaded();
    library
        .verify(&contract(library.content_hash()))
        .expect("the fixture matches its own contract");
}

/// Bytes that are not a module are refused where an embedder can still say
/// something useful about them — at load, not at first call.
#[test]
fn something_that_is_not_a_module_is_refused_at_load() {
    let error = Library::from_bytes(b"not a kira module at all").expect_err("it must refuse");
    assert!(matches!(error, Error::Decode(_)), "got {error:?}");
}

#[test]
fn a_truncated_artifact_is_refused_rather_than_partly_read() {
    let bytes = artifact();
    let error = Library::from_bytes(&bytes[..bytes.len() / 2]).expect_err("it must refuse");
    assert!(matches!(error, Error::Decode(_)), "got {error:?}");
}

/// A module whose export table claims a function it does not have never becomes
/// a `Library` — the validation that proves it is structural, and it runs here
/// rather than at the call that would have entered the wrong frame.
#[test]
fn a_module_whose_export_table_lies_is_refused_at_load() {
    let mut module = fixture_library();
    module.exports.functions[0].function = 99;
    let error = Library::from_bytes(&module.to_bytes()).expect_err("it must refuse");
    assert!(matches!(error, Error::Invalid(_)), "got {error:?}");
}

#[test]
fn a_missing_export_names_itself() {
    let mut module = fixture_library();
    module.exports.functions.remove(1);
    let library = Library::from_bytes(&module.to_bytes()).expect("still a valid library");
    let error = library
        .verify(&contract(library.content_hash()))
        .expect_err("the wrapper calls something that is gone");
    assert_eq!(
        error,
        Error::Contract(ContractError::MissingExport {
            name: "button_label".to_owned(),
        })
    );
}

#[test]
fn an_export_that_grew_a_parameter_names_itself_and_both_counts() {
    let mut module = fixture_library();
    module.exports.functions[0].params.push(ExportType::Int);
    module.functions[0].param_count = 2;
    module.functions[0].local_count = 2;
    let library = Library::from_bytes(&module.to_bytes()).expect("still a valid library");
    let error = library
        .verify(&contract(library.content_hash()))
        .expect_err("the wrapper passes one argument, the library takes two");
    assert_eq!(
        error,
        Error::Contract(ContractError::Arity {
            export: "make_button".to_owned(),
            expected: 1,
            found: 2,
        })
    );
}

#[test]
fn a_parameter_whose_type_changed_names_both_types() {
    let mut module = fixture_library();
    module.exports.functions[0].params[0] = ExportType::Int;
    let library = Library::from_bytes(&module.to_bytes()).expect("still a valid library");
    let error = library
        .verify(&contract(library.content_hash()))
        .expect_err("a string became an integer");
    assert_eq!(
        error,
        Error::Contract(ContractError::ParamType {
            export: "make_button".to_owned(),
            position: 0,
            expected: "a string".to_owned(),
            found: "an integer".to_owned(),
        })
    );
}

#[test]
fn a_result_whose_type_changed_names_both_types() {
    let mut module = fixture_library();
    module.exports.functions[1].result = ExportType::Int;
    let library = Library::from_bytes(&module.to_bytes()).expect("still a valid library");
    let error = library
        .verify(&contract(library.content_hash()))
        .expect_err("a string result became an integer");
    assert_eq!(
        error,
        Error::Contract(ContractError::ResultType {
            export: "button_label".to_owned(),
            expected: "a string".to_owned(),
            found: "an integer".to_owned(),
        })
    );
}

/// The reason classes are compared before signatures: a handle's type is an
/// index into this list, so two classes swapping places is a real type error
/// with every name still spelled correctly.
#[test]
fn a_renamed_class_is_caught_before_any_signature_is_compared() {
    let mut module = fixture_library();
    module.exports.classes[0] = "Window".to_owned();
    let library = Library::from_bytes(&module.to_bytes()).expect("still a valid library");
    let error = library
        .verify(&contract(library.content_hash()))
        .expect_err("the wrapper's `Button` newtype names nothing here");
    assert_eq!(
        error,
        Error::Contract(ContractError::ClassName {
            position: 0,
            expected: "Button".to_owned(),
            found: "Window".to_owned(),
        })
    );
}

#[test]
fn a_class_that_appeared_is_caught_by_count() {
    let mut module = fixture_library();
    module.exports.classes.push("Window".to_owned());
    let library = Library::from_bytes(&module.to_bytes()).expect("still a valid library");
    let error = library
        .verify(&contract(library.content_hash()))
        .expect_err("the wrapper knows one class");
    assert_eq!(
        error,
        Error::Contract(ContractError::ClassCount {
            expected: 1,
            found: 2,
        })
    );
}

/// The catch-all: a library whose surface is identical but whose *bytes* are
/// not the ones the wrapper was generated from. Nothing structural is wrong, so
/// only the hash can say so.
#[test]
fn a_rebuilt_library_with_the_same_surface_is_still_caught_by_its_hash() {
    let library = loaded();
    let error = library
        .verify(&contract(0xdead_beef_dead_beef))
        .expect_err("the wrapper was generated from other bytes");
    assert_eq!(
        error,
        Error::Contract(ContractError::ContentHash {
            expected: 0xdead_beef_dead_beef,
            found: library.content_hash(),
        })
    );
}

/// The structural check is what a reader can act on, so it must win over the
/// hash — which is also wrong, and says the least about which change matters.
#[test]
fn a_structural_disagreement_is_reported_ahead_of_the_hash() {
    let mut module = fixture_library();
    module.exports.functions.remove(1);
    let library = Library::from_bytes(&module.to_bytes()).expect("still a valid library");
    let error = library
        .verify(&contract(0xdead_beef_dead_beef))
        .expect_err("both checks fail");
    assert!(
        matches!(error, Error::Contract(ContractError::MissingExport { .. })),
        "the hash drowned out the useful answer: {error}"
    );
}

/// A library exporting more than the wrapper calls is not a mismatch: the
/// wrapper calls by name and simply has no method for the extra one. The hash
/// still notices, and says so as a stale build.
#[test]
fn an_export_the_wrapper_does_not_know_is_not_a_signature_mismatch() {
    let library = loaded();
    let narrow = ExportContract {
        classes: &["Button"],
        functions: &[ExpectedExport {
            name: "greet",
            params: &[ExportType::String],
            result: ExportType::Void,
        }],
        content_hash: library.content_hash(),
    };
    library.verify(&narrow).expect("a subset is honoured");
}

#[test]
fn the_content_hash_is_a_function_of_the_bytes_alone() {
    assert_eq!(content_hash(b""), 0xcbf2_9ce4_8422_2325);
    assert_eq!(content_hash(b"kira"), content_hash(b"kira"));
    assert_ne!(content_hash(b"kira"), content_hash(b"kirb"));
    // Order matters, which a summing hash would not give.
    assert_ne!(content_hash(b"ab"), content_hash(b"ba"));
}

#[test]
fn one_library_instantiates_independently_any_number_of_times() {
    let library = loaded();
    let mut first = library.instantiate().expect("it loads");
    let mut second = library.instantiate().expect("it loads again");

    let made = first
        .call("make_button", &[NativeArg::Str("ok")])
        .expect("clean call");
    let NativeResult::Handle(word) = made else {
        panic!("a class result crosses as a handle, got {made:?}")
    };
    assert_eq!(first.live_handles(), 1);
    assert_eq!(second.live_handles(), 0, "the instances share nothing");

    // The other instance has never minted this root, and says so by name rather
    // than resolving the word against whatever it does hold.
    let error = second
        .call("button_label", &[NativeArg::Handle(word)])
        .expect_err("another instance's handle names nothing here");
    assert!(
        matches!(
            error,
            Error::Vm(kira_vm_runtime::VmError::DanglingRoot { .. })
        ),
        "got {error:?}"
    );

    first.release(Handle::from_word(word)).expect("it was live");
    assert_eq!(first.finish().current, 0);
    assert_eq!(second.finish().current, 0);
}
