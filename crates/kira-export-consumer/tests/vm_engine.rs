//! What the VM engine offers that the native engine does not.
//!
//! Two things, and both are consequences of *where the library runs* rather than
//! gaps to be closed:
//!
//! - **A host.** The VM formats `print` into finished lines and hands them to a
//!   `HostCapabilities` the embedder supplied, so a consumer can redirect the
//!   library's output into a log or a test buffer. Native code calls the
//!   runtime's printer directly; giving it a host would be an ABI decision, not
//!   a parameter, and nobody has needed one.
//! - **Live-handle accounting.** The VM owns a heap it can count. The native
//!   engine's handles are boxes from the system allocator, and counting them
//!   would mean the runtime keeping a registry that exists only to be asserted
//!   on.
//!
//! Everything a consumer does against *both* engines is in `consumer.rs`, which
//! is compiled unchanged against each. This file is the remainder, stated
//! rather than hidden.

#![cfg(not(feature = "native-engine"))]

use kira_export_consumer::GENERATED_CRATE;
use kira_export_consumer::uifoundation::Uifoundation;
use kira_runtime_abi::CapturingHost;

/// Loads the library, failing the test with the reason if it will not load.
fn load() -> Uifoundation {
    match Uifoundation::load() {
        Ok(library) => library,
        Err(error) => panic!("the embedded library did not load: {error}"),
    }
}

#[test]
fn dropping_a_handle_releases_the_object_it_names() {
    let ui = load();
    assert_eq!(ui.live_handles(), 0);
    let first = ui.make_button("one").expect("make_button");
    let second = ui.make_button("two").expect("make_button");
    assert_eq!(ui.live_handles(), 2);
    drop(second);
    assert_eq!(ui.live_handles(), 1);
    drop(first);
    assert_eq!(ui.live_handles(), 0);
}

#[test]
fn many_handles_made_and_dropped_leave_nothing_behind() {
    // The transient-string and handle disciplines are only worth anything if
    // they hold under repetition; a leak of one object per call is invisible in
    // a single-call test and fatal in a UI.
    let ui = load();
    for index in 0..300 {
        let button = ui.make_button("ok").expect("make_button");
        assert_eq!(ui.button_label(&button).expect("label"), "ok");
        assert_eq!(ui.live_handles(), 1, "at iteration {index}");
    }
    assert_eq!(ui.live_handles(), 0);
}

#[test]
fn two_instances_of_one_library_share_nothing() {
    let first = load();
    let second = load();
    let button = first.make_button("ok").expect("make_button");
    assert_eq!(first.live_handles(), 1);
    assert_eq!(second.live_handles(), 0);
    drop(button);
    assert_eq!(first.live_handles(), 0);
}

#[test]
fn the_embedder_chooses_where_the_library_prints() {
    // The design's contract for the VM engine: the wrapper provides a default
    // host and accepts a custom one. Nothing is asserted about stdout here —
    // the lines are read back out of the host the consumer supplied, which is
    // the only way to tell a redirected effect from one that went nowhere.
    let ui = Uifoundation::load_with(CapturingHost::new()).expect("load_with");
    let button = ui.make_button("ok").expect("make_button");
    ui.announce(&button).expect("announce");
    ui.announce(&button).expect("announce");
    ui.with_host(|host| {
        assert_eq!(
            host.lines(),
            ["showing ok".to_owned(), "showing ok".to_owned()]
        );
    });
    // And the same handles and accounting still work through a custom host.
    assert_eq!(ui.button_label(&button).expect("label"), "ok");
    drop(button);
    assert_eq!(ui.live_handles(), 0);
}

#[test]
fn a_host_can_be_drained_between_calls() {
    let ui = Uifoundation::load_with(CapturingHost::new()).expect("load_with");
    let button = ui.make_button("save").expect("make_button");
    ui.announce(&button).expect("announce");
    let first = ui.with_host_mut(std::mem::take);
    ui.announce(&button).expect("announce");
    let second = ui.with_host_mut(std::mem::take);
    assert_eq!(first.into_output(), "showing save\n");
    assert_eq!(second.into_output(), "showing save\n");
}

#[test]
fn the_default_host_is_still_what_load_gives() {
    // `load()` needs no type annotation and no host: the generic parameter
    // defaults, so the simple case did not get harder to buy the flexible one.
    let ui: Uifoundation = load();
    let button = ui.make_button("ok").expect("make_button");
    ui.announce(&button).expect("announce");
}

#[test]
fn the_generated_code_contains_no_unsafe() {
    // The VM engine's claim to a consumer is: no linker, no LLVM, no unsafe.
    // The first two are proved by this crate building at all on a machine with
    // neither; this is the third.
    let source = std::fs::read_to_string(
        std::path::Path::new(GENERATED_CRATE)
            .join("src")
            .join("lib.rs"),
    )
    .expect("read the generated wrapper");
    let code: String = source
        .lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(!code.contains("unsafe"), "{code}");
}

#[test]
fn the_generated_crate_carries_the_bytecode_it_embeds() {
    // The VM engine's artifact is data inside the crate, which is what makes its
    // stale-build guard a content hash rather than a symbol.
    let root = std::path::Path::new(GENERATED_CRATE);
    assert!(root.join("uifoundation.kbc").is_file(), "{root:?}");
}
