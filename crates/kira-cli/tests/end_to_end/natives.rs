//! `@Native` in a library remains valid on the VM engine.
//!
//! The VM compiles every function to bytecode, so the annotation does not
//! request native execution in this backend. These tests cover library build
//! and application execution.

use crate::{LIBRARY_SOURCE, kira, run_source, write_package};

/// A library with one `@Native` function among ordinary ones.
const NATIVE_LIBRARY: &str = "\
@Export\n\
function add(a: Int, b: Int) -> Int { return a + b }\n\
@Native\n\
function fast(value: Int) -> Int { return value * 2 }";

#[test]
fn a_native_function_does_not_stop_a_vm_library_building() {
    let path = write_package(".Library", NATIVE_LIBRARY);
    let output = kira(&["build", "--backend", "vm", path.to_str().unwrap()]);
    let _ = std::fs::remove_dir_all(path.parent().expect("package directory"));
    assert!(
        output.status.success(),
        "the vm engine refused a `@Native` library: {}",
        String::from_utf8_lossy(&output.stderr),
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("Successfully built"),
        "{:?}",
        output.stdout,
    );
}

#[test]
fn a_native_function_in_an_application_is_compiled_to_bytecode() {
    // The mechanism the test above depends on, exercised where it is
    // observable: the body really does run, on the VM, producing an answer. An
    // engine that had quietly skipped it would return unit instead.
    let output = run_source(
        "@Native\nfunction fast(value: Int) -> Int { return value * 2 }\n\
         @Main function main() { print(fast(21)) return }",
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout), "42\n");
}

#[test]
fn a_library_that_exports_nothing_still_builds() {
    // Library validity is independent of whether it declares an export.
    let path = write_package(".Library", LIBRARY_SOURCE);
    let output = kira(&["build", "--backend", "vm", path.to_str().unwrap()]);
    let _ = std::fs::remove_dir_all(path.parent().expect("package directory"));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stderr}");
}
