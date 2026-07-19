//! What the **hybrid engine's** generator emits, and what it shares.
//!
//! Split from the VM engine's tests by the file-size ladder. The first test here
//! is the one that matters most: both engines come out of one renderer, so what
//! is pinned is that the difference between them is confined to loading.

use std::path::Path;

use kira_bytecode::ExportTable;

use super::vm::{lib_rs, uifoundation};
use crate::wrapper::{GeneratedCrate, WrapperSpec, generate_hybrid};

/// The hybrid crate for the motivating library, at a known native path.
fn generated_hybrid(exports: &ExportTable) -> GeneratedCrate {
    generate_hybrid(
        &WrapperSpec {
            library: "uifoundation",
            version: "0.1.0",
            exports,
            content_hash: 0x0123_4567_89ab_cdef,
            toolchain_root: Path::new("/kira"),
        },
        "uifoundation.khm",
        Path::new("/build/lib/libuifoundation.dylib"),
    )
    .expect("generate")
}

fn hybrid_lib_rs(exports: &ExportTable) -> String {
    generated_hybrid(exports)
        .file("src/lib.rs")
        .expect("lib.rs")
        .to_owned()
}

#[test]
fn the_consumer_facing_api_is_the_vm_engines_character_for_character() {
    // The claim this whole engine is measured against. Both crates come out
    // of one renderer, so what is pinned here is that the *difference* is
    // confined to loading — every method, every newtype, every signature is
    // the same text.
    let table = uifoundation();
    let vm = lib_rs(&table);
    let hybrid = hybrid_lib_rs(&table);
    for item in [
        "pub fn make_button(&self, arg0: &str) -> Result<Button<H>, Error> {",
        "pub fn button_width(&self, arg0: &Button<H>) -> Result<i64, Error> {",
        "pub fn button_label(&self, arg0: &Button<H>) -> Result<String, Error> {",
        "pub fn load_with(host: H) -> Result<Uifoundation<H>, Error> {",
        "pub fn with_host<R>(&self, read: impl FnOnce(&H) -> R) -> R {",
        "pub fn live_handles(&self) -> usize {",
        "impl<H: HostCapabilities> Drop for Button<H> {",
    ] {
        assert!(vm.contains(item), "the VM crate lost `{item}`");
        assert!(hybrid.contains(item), "the hybrid crate has no `{item}`");
    }
}

#[test]
fn it_embeds_both_halves_descriptions_and_points_at_the_third() {
    let source = hybrid_lib_rs(&uifoundation());
    assert!(
        source.contains("include_bytes!(\"../uifoundation.kbc\")"),
        "{source}"
    );
    assert!(
        source.contains("include_bytes!(\"../uifoundation.khm\")"),
        "{source}"
    );
    // The shared library is a path rather than bytes, because it is loaded
    // rather than embedded — and it is a raw string, so a Windows-shaped
    // path would not become an escape sequence.
    assert!(
        source.contains("const NATIVE_HALF: &str = r\"/build/lib/libuifoundation.dylib\";"),
        "{source}"
    );
    assert!(
        source.contains("HybridLibrary::from_parts(LIBRARY_NAME, BYTECODE, MANIFEST)?"),
        "{source}"
    );
}

#[test]
fn it_keeps_the_same_stale_build_guard_the_vm_engine_has() {
    // The bytecode half is a VM-engine library, so the content hash is the
    // guard here too — and it has to still be emitted, or a hybrid wrapper
    // would call a `.kbc` it was not generated from.
    let source = hybrid_lib_rs(&uifoundation());
    assert!(
        source.contains("content_hash: 0x0123456789abcdef,"),
        "{source}"
    );
    assert!(source.contains("library.verify(&CONTRACT)?;"), "{source}");
}

#[test]
fn it_contains_no_unsafe() {
    // Every `dlopen`, symbol bind, and crossing is behind `kira-hybrid-main`,
    // so the generated file keeps the VM engine's promise unchanged.
    let source = hybrid_lib_rs(&uifoundation());
    let code: String = source
        .lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(!code.contains("unsafe"), "{code}");
}

#[test]
fn its_manifest_names_the_hybrid_embedding_crate_and_no_build_script() {
    let manifest = generated_hybrid(&uifoundation())
        .file("Cargo.toml")
        .expect("Cargo.toml")
        .to_owned();
    assert!(manifest.contains("kira-hybrid-main = {"), "{manifest}");
    // `kira-main` is reached through `kira-hybrid-main`, which re-exports
    // the contract types, so the generated crate names one engine crate.
    assert!(!manifest.contains("\nkira-main = {"), "{manifest}");
    // Nothing is linked, so cargo must not go looking for a build script the
    // native engine may have left in this same directory.
    assert!(manifest.contains("\nbuild = false\n"), "{manifest}");
    // And the generated code still has no unsafe to permit.
    assert!(manifest.contains("unsafe_code = \"forbid\""), "{manifest}");
}

#[test]
fn its_readme_states_the_deployment_story_rather_than_assuming_one() {
    // The one engine whose artifact does not travel entirely inside the
    // consumer's binary, so the one engine that owes an answer to "what do I
    // ship" — in the crate the consumer depends on.
    let readme = generated_hybrid(&uifoundation())
        .file("README.md")
        .expect("README.md")
        .to_owned();
    assert!(readme.contains("libuifoundation."), "{readme}");
    assert!(readme.contains("KIRA_UIFOUNDATION_NATIVE"), "{readme}");
    assert!(readme.contains("Beside your own executable"), "{readme}");
    assert!(
        readme.contains("/build/lib/libuifoundation.dylib"),
        "{readme}"
    );
    // And the two costs, named rather than discovered later.
    assert!(readme.contains("libloading"), "{readme}");
    assert!(readme.contains("wasm32-unknown-unknown"), "{readme}");
}

#[test]
fn generation_is_deterministic() {
    // Regenerated on every build, so a crate that differed run to run would
    // rebuild a consumer's world for nothing.
    let table = uifoundation();
    assert_eq!(generated_hybrid(&table), generated_hybrid(&table));
}
