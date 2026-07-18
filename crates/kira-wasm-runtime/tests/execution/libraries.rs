//! The wasm backend's library refusal.
//!
//! A wasm module is entered at one exported entrypoint. A library has none, so
//! the backend refuses by name rather than emitting a module nothing can start
//! — which is the shape of every "not built yet" answer in this repo: a typed
//! refusal with a stated reason, never a silent gap.

use super::*;

#[test]
fn a_library_is_refused_by_name_on_both_devices() {
    // Analyzed as a library, so it lowers with no entrypoint — the same IR
    // `kirac build` hands the backend for a `kind = .Library` package.
    let db = salsa::DatabaseImpl::new();
    let program = kira_semantics::SourceProgram::new(
        &db,
        "function add(a: Int, b: Int) -> Int { return a + b }".to_owned(),
        "test.kira".to_owned(),
        Vec::new(),
        kira_semantics::BuildKind::Library,
    );
    let ir = kira_ir::lower(&kira_semantics::analyzed(&db, program));
    assert_eq!(ir.main, None, "a library carries no entrypoint");

    for device in [WasmDevice::Wasm32, WasmDevice::Wasm64] {
        let error = kira_wasm_runtime::compile(&ir, device).expect_err("a library is refused");
        assert!(
            matches!(error, kira_wasm_runtime::WasmError::LibraryUnsupported),
            "{device:?} refused for a different reason: {error:?}",
        );
        // The reason travels with the refusal, so a user reading it learns what
        // is missing rather than only that something is.
        let message = error.to_string();
        assert!(
            message.contains("a library cannot be built as a wasm module yet"),
            "{message}"
        );
        assert!(message.contains("undesigned"), "{message}");
    }
}

#[test]
fn a_program_with_an_entrypoint_still_compiles_on_both_devices() {
    // The negative test's control: the refusal is about a missing entrypoint
    // and nothing else, so an ordinary program is unaffected.
    let ir = lower("@Main function main() { print(1) return }");
    for device in [WasmDevice::Wasm32, WasmDevice::Wasm64] {
        assert!(
            kira_wasm_runtime::compile(&ir, device).is_ok(),
            "{device:?} refused a program with an entrypoint",
        );
    }
}
