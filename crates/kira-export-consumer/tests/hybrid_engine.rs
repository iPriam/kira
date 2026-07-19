//! What the hybrid engine offers that neither other engine does.
//!
//! `consumer.rs` is the parity proof and runs unchanged against all three
//! engines; `vm_engine.rs` is what the two VM-family engines share. This file is
//! the remainder that is *only* true here, and it is short on purpose — a long
//! one would mean the engine had grown a surface of its own, which is the thing
//! the generated API exists to prevent.
//!
//! Two things:
//!
//! - **The split is real.** `widen` is bytecode and calls `widenedWidth`, which
//!   is machine code in a shared library this process opened. On the other two
//!   engines the annotation is ignored and both functions run on one engine.
//! - **There is a file to deploy.** The other two engines put the whole library
//!   inside the consumer's binary. This one does not, and cannot: what it costs
//!   and how the file is found is a designed search order rather than an
//!   assumption.
//!
//! What that search order does when the file is *absent* is in
//! `hybrid_missing_native.rs`, which is its own test binary for a reason stated
//! there.

#![cfg(feature = "hybrid-engine")]

use kira_export_consumer::GENERATED_CRATE;
use kira_export_consumer::uifoundation::Uifoundation;

/// Loads the library, failing the test with the reason if it will not load.
fn load() -> Uifoundation {
    match Uifoundation::load() {
        Ok(library) => library,
        Err(error) => panic!("the hybrid library did not load: {error}"),
    }
}

#[test]
fn a_bytecode_export_calls_a_native_function_and_gets_the_same_answer() {
    // The engine's whole reason to exist, exercised: the consumer calls
    // `widen`, which is `@Runtime` bytecode; that bytecode crosses the seam into
    // `widenedWidth`, which is `@Native` machine code in the loaded shared
    // library; the result comes back through the seam and out as a handle.
    //
    // `consumer.rs` asserts the same numbers against all three engines. What is
    // added here is that on *this* engine the number came through machine code
    // on the way — which is why the loop runs rather than checking once: a seam
    // that leaked a string or a value per crossing would survive one call.
    let ui = load();
    let button = ui.make_button("ok").expect("make_button");
    for by in 0..200 {
        let wider = ui.widen(&button, by).expect("widen");
        assert_eq!(ui.button_width(&wider).expect("width"), 120 + by);
        assert_eq!(ui.button_label(&wider).expect("label"), "ok");
    }
    // And the instance is still balanced afterwards: crossing the seam must not
    // root anything the consumer did not ask for.
    drop(button);
    assert_eq!(ui.live_handles(), 0);
}

#[test]
fn the_generated_crate_embeds_both_halves_descriptions() {
    // Two of the three artifacts are data and live inside the crate, which is
    // what makes the crate relocatable and the deployment exactly one file.
    let root = std::path::Path::new(GENERATED_CRATE);
    for file in ["uifoundation.kbc", "uifoundation.khm"] {
        assert!(root.join(file).is_file(), "{file} is missing from {root:?}");
    }
    // And no build script: this engine links nothing, so a `build.rs` here would
    // be the native engine's leftover, which cargo would run.
    assert!(!root.join("build.rs").exists(), "{root:?}");
}

#[test]
fn the_readme_names_the_file_a_deployment_has_to_carry() {
    // The deployment story is designed rather than assumed, and the place that
    // has to state it is the crate the consumer depends on — not a note in the
    // compiler's repository, which they will never read.
    let readme = std::fs::read_to_string(std::path::Path::new(GENERATED_CRATE).join("README.md"))
        .expect("read README");
    assert!(
        readme.contains(&kira_hybrid_main::shared_library_file_name("uifoundation")),
        "{readme}"
    );
    assert!(
        readme.contains(&kira_hybrid_main::override_variable("uifoundation")),
        "{readme}"
    );
    assert!(readme.contains("Beside your own executable"), "{readme}");
    // And it says what this engine costs, rather than letting `libloading` and
    // the wasm gap arrive unannounced in someone's dependency tree.
    assert!(readme.contains("libloading"), "{readme}");
    assert!(readme.contains("wasm32-unknown-unknown"), "{readme}");
}

#[test]
fn the_generated_code_contains_no_unsafe() {
    // The same claim the VM engine makes, and it survives this engine: every
    // `dlopen`, every symbol bind, and every crossing is behind
    // `kira-hybrid-main`, so the file a consumer compiles has none of it.
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
