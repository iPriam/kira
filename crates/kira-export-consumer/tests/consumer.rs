//! `uifoundation::make_button("ok")`, end to end — **against either engine**.
//!
//! Every test here is a Rust program calling a library authored in Kira,
//! through code nobody wrote by hand. Nothing is stubbed and nothing is
//! asserted about the compiler: the values come back out of the library.
//!
//! # This file is the parity proof
//!
//! It is compiled and run twice, unchanged, against two completely different
//! engines: the VM (`cargo test -p kira-export-consumer`, no LLVM anywhere) and
//! native machine code (`--features native-engine`, LLVM-gated). Not two
//! similar suites — the *same* file, because the feature's central claim is that
//! a consumer's code does not change when the engine does. A test that had to be
//! edited to run against the second engine would have disproved the claim it was
//! written to check.
//!
//! Anything one engine has and the other does not — the VM's custom hosts and
//! live-handle accounting — lives in `vm_engine.rs` instead, where it cannot
//! quietly weaken this file.

use kira_export_consumer::GENERATED_CRATE;
use kira_export_consumer::uifoundation::{Button, Uifoundation};

/// Loads the library, failing the test with the reason if it will not load.
fn load() -> Uifoundation {
    match Uifoundation::load() {
        Ok(library) => library,
        Err(error) => panic!("the library did not load: {error}"),
    }
}

#[test]
fn a_rust_program_makes_a_button_and_reads_it_back() {
    let ui = load();
    let button: Button = ui.make_button("ok").expect("make_button");
    assert_eq!(ui.button_label(&button).expect("button_label"), "ok");
    assert_eq!(ui.button_width(&button).expect("button_width"), 120);
}

#[test]
fn a_string_result_is_owned_by_the_caller() {
    let ui = load();
    let button = ui.make_button("save changes").expect("make_button");
    // The `String` outlives the call it came from, which is the whole of the
    // Kira-to-Rust half of the boundary contract.
    let label: String = ui.button_label(&button).expect("button_label");
    drop(button);
    assert_eq!(label, "save changes");
}

#[test]
fn rust_re_enters_kira_once_per_event() {
    // What a "callback" means in this direction: the Rust side owns the event
    // loop and calls back into the library per event. No machinery, just calls.
    let ui = load();
    let button = ui.make_button("ok").expect("make_button");
    let events = [(4, 8), (-1, 0), (119, 0), (120, 0), (0, -3)];
    let hits: Vec<bool> = events
        .iter()
        .map(|(x, y)| ui.click_at(&button, *x, *y).expect("click_at"))
        .collect();
    assert_eq!(hits, [true, false, true, false, false]);
}

#[test]
fn a_handle_argument_is_lent_and_a_handle_result_is_owned() {
    let ui = load();
    let button = ui.make_button("ok").expect("make_button");
    let wider = ui.widen(&button, 30).expect("widen");
    // The button that was lent is unchanged; the one that came back is new.
    assert_eq!(ui.button_width(&button).expect("width"), 120);
    assert_eq!(ui.button_width(&wider).expect("width"), 150);
    assert_eq!(ui.button_label(&wider).expect("label"), "ok");
}

#[test]
fn an_export_with_no_arguments_and_one_returning_a_float_both_work() {
    let ui = load();
    assert_eq!(ui.default_width().expect("default_width"), 120);
    let button = ui.make_button("ok").expect("make_button");
    assert!((ui.aspect_ratio(&button).expect("aspect_ratio") - 0.5).abs() < f64::EPSILON);
}

#[test]
fn only_exported_functions_appear_on_the_surface() {
    // `hidden()` is in the library and is not marked `@Export`. Nothing about
    // it reaches Rust — there is no method for it, which the compiler already
    // enforced by compiling this file, so what is checked here is that it did
    // not reach the artifact's export table either.
    let source = std::fs::read_to_string(
        std::path::Path::new(GENERATED_CRATE)
            .join("src")
            .join("lib.rs"),
    )
    .expect("read the generated wrapper");
    assert!(!source.contains("hidden"), "{source}");
    assert!(source.contains("pub fn make_button"), "{source}");
}

#[test]
fn many_handles_and_strings_made_and_dropped_leave_the_library_balanced() {
    // The transient-string and handle disciplines are only worth anything under
    // repetition: a leak of one object per call is invisible in a single-call
    // test and fatal in a UI. What each engine leaks differently is what makes
    // this worth running on both — the VM would grow its heap, the native
    // engine would grow the process's.
    //
    // The VM engine additionally asserts the count itself, in `vm_engine.rs`.
    // Here the loop is the test: it drives allocate-and-release 300 times
    // through whichever engine is linked, and a discipline that is wrong on
    // either side shows up as a crash or a double free rather than a number.
    let ui = load();
    for index in 0..300 {
        let button = ui.make_button("ok").expect("make_button");
        assert_eq!(ui.button_label(&button).expect("label"), "ok", "at {index}");
        let wider = ui.widen(&button, index).expect("widen");
        assert_eq!(
            ui.button_width(&wider).expect("width"),
            120 + index,
            "at {index}"
        );
    }
}

#[test]
fn a_handle_outlives_the_binding_that_made_it() {
    // The whole reason a UI library is possible: a button made in one call has
    // to still be there in the next one. On the VM that is a rooted heap; on the
    // native engine it is a box the destructor owns. The consumer sees neither.
    let ui = load();
    let button = {
        // The scope is the point: the binding that made the button is gone
        // before it is used again.
        ui.make_button("ok").expect("make_button")
    };
    for _ in 0..64 {
        assert_eq!(ui.button_width(&button).expect("width"), 120);
    }
    assert_eq!(ui.button_label(&button).expect("label"), "ok");
}

#[test]
fn a_handle_names_the_library_it_came_from() {
    let ui = load();
    let button = ui.make_button("ok").expect("make_button");
    let same = button.library();
    assert_eq!(same.button_label(&button).expect("label"), "ok");
}

#[test]
fn the_generated_crate_is_a_crate_and_says_not_to_commit_it() {
    // Whichever engine wrote it: a manifest, a README that says not to commit
    // it, and the source. The artifact beside them differs by engine, and is
    // checked where that difference is meaningful.
    let root = std::path::Path::new(GENERATED_CRATE);
    for file in ["Cargo.toml", "README.md", "src/lib.rs"] {
        assert!(root.join(file).is_file(), "{file} is missing from {root:?}");
    }
    let readme = std::fs::read_to_string(root.join("README.md")).expect("read README");
    assert!(readme.contains("do not commit"), "{readme}");
}
