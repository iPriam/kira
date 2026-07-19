//! Library packages.
//!
//! `kind = .Library` in `package.kira` is what makes a package a library, and
//! these prove the three things that follow from it end to end, through the real
//! binary: a library with no `@Main` checks clean, running one is refused by
//! name, and building one produces an artifact on each backend the CI machine
//! has.

use crate::{LIBRARY_SOURCE, check_source, kirac, write_package};

#[test]
fn a_library_without_main_checks_clean() {
    let path = write_package(".Library", LIBRARY_SOURCE);
    let output = kirac(&["check", path.to_str().unwrap()]);
    let _ = std::fs::remove_dir_all(path.parent().expect("package directory"));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stderr}");
    assert!(
        !stderr.contains("KSEM011"),
        "a library needs no `@Main`: {stderr}"
    );
}

#[test]
fn the_same_source_in_an_app_package_is_still_ksem011() {
    // The exemption comes from the manifest and nowhere else. Same bytes, same
    // command, different `kind` — and the entrypoint requirement comes back.
    let path = write_package(".App", LIBRARY_SOURCE);
    let output = kirac(&["check", path.to_str().unwrap()]);
    let _ = std::fs::remove_dir_all(path.parent().expect("package directory"));
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("KSEM011"), "{stderr}");
}

#[test]
fn a_library_declaring_main_is_refused() {
    let path = write_package(".Library", "@Main function main() { print(1) return }");
    let output = kirac(&["check", path.to_str().unwrap()]);
    let _ = std::fs::remove_dir_all(path.parent().expect("package directory"));
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("KSEM158"), "{stderr}");
}

#[test]
fn running_a_library_is_refused_by_name_with_a_reason() {
    let path = write_package(".Library", LIBRARY_SOURCE);
    let output = kirac(&["run", path.to_str().unwrap()]);
    let _ = std::fs::remove_dir_all(path.parent().expect("package directory"));
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("cannot run a library"), "{stderr}");
    // The reason, not just the refusal: a user who is told "no" and not "why"
    // has to guess.
    assert!(stderr.contains("no `@Main` entrypoint"), "{stderr}");
}

#[test]
fn a_library_builds_on_the_vm_backend() {
    // The VM backend is the one CI has, so this is the artifact proof that runs
    // everywhere. It compiles to a real KBC1 module with no entrypoint.
    let path = write_package(".Library", LIBRARY_SOURCE);
    let output = kirac(&["build", "--backend", "vm", path.to_str().unwrap()]);
    let _ = std::fs::remove_dir_all(path.parent().expect("package directory"));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stderr}");
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("Successfully built"),
        "{:?}",
        output.stdout
    );
}

#[test]
fn a_library_cannot_be_built_for_the_web_and_says_why() {
    // The recorded wasm refusal: a library artifact for a JS host needs a
    // string/allocator contract across the module boundary that is undesigned.
    let path = write_package(".Library", LIBRARY_SOURCE);
    let output = kirac(&[
        "build",
        "--backend",
        "llvm",
        "--device",
        "wasm32",
        path.to_str().unwrap(),
    ]);
    let _ = std::fs::remove_dir_all(path.parent().expect("package directory"));
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("a library cannot be built as a wasm module yet"),
        "{stderr}"
    );
}

#[test]
fn a_package_with_no_manifest_is_still_an_application() {
    // The default has to hold: a bare `.kira` file is a program, so a missing
    // `@Main` is still an error with no manifest anywhere above it.
    let output = check_source("function add(a: Int, b: Int) -> Int { return a + b }");
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("KSEM011"), "{stderr}");
}

#[test]
fn a_malformed_package_manifest_is_reported_not_ignored() {
    let path = write_package(".Plugin", LIBRARY_SOURCE);
    let output = kirac(&["check", path.to_str().unwrap()]);
    let _ = std::fs::remove_dir_all(path.parent().expect("package directory"));
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("is not a package kind"), "{stderr}");
}
