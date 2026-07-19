//! What a consumer sees when the native half is not there.
//!
//! # Why this is its own test binary
//!
//! It sets an environment variable, and an environment variable is process-wide.
//! Cargo runs the tests within one binary on parallel threads, so doing this
//! beside the other hybrid tests would change what *they* load, non-
//! deterministically — which is exactly how it failed the first time it was
//! written. A file of its own is a binary of its own, and a binary with one test
//! in it has nothing to race.
//!
//! The alternative — asserting against `kira_hybrid_main::locate` directly —
//! would test the search but not the message, and the message is the deliverable
//! here: this engine is the only one whose artifact can be missing at run time,
//! so what it says when it is missing is part of the feature.

#![cfg(feature = "hybrid-engine")]

use kira_export_consumer::GENERATED_CRATE;
use kira_export_consumer::uifoundation::Uifoundation;

#[test]
fn a_missing_native_half_is_a_typed_error_naming_every_path_tried() {
    // Driven through the real `load()`, so this is the message a consumer sees.
    // Pointing the override at nothing also pins the decision that makes the
    // override worth having: it does **not** fall through. The build
    // directory's copy is right there and would load fine — and using it would
    // hand an operator a library they did not name.
    let missing = std::path::Path::new(GENERATED_CRATE).join("no-such-native-half.dylib");
    let variable = kira_hybrid_main::override_variable("uifoundation");
    // SAFETY: this binary contains exactly this one test, so no other thread is
    // reading the environment while it is being written. That is the whole
    // reason the test lives in a file of its own; see the module docs.
    unsafe { std::env::set_var(&variable, &missing) };

    let error = Uifoundation::load()
        .err()
        .expect("a native half that is not there");
    let message = error.to_string();
    assert!(message.contains("uifoundation"), "{message}");
    assert!(message.contains("no-such-native-half"), "{message}");
    assert!(message.contains(&variable), "{message}");
    assert!(message.contains("tried, in order"), "{message}");
}
