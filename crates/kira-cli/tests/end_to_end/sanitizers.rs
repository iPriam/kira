//! AddressSanitizer command-line and managed-runtime contracts.

use std::process::Command;

use crate::write_isolated_source;

const PROGRAM: &str = "@Main function main() { print(1) return }";

#[test]
fn address_sanitizer_never_falls_back_to_a_host_compiler_runtime() {
    let source = write_isolated_source(PROGRAM);
    let fake_llvm = source
        .parent()
        .expect("isolated source directory")
        .join("llvm-without-compiler-rt");
    std::fs::create_dir_all(fake_llvm.join("include/llvm-c")).expect("LLVM include directory");
    std::fs::create_dir_all(fake_llvm.join("bin")).expect("LLVM bin directory");
    std::fs::write(fake_llvm.join("include/llvm-c/Core.h"), b"").expect("LLVM marker header");
    std::fs::write(
        fake_llvm
            .join("bin")
            .join(kira_toolchain::executable_name("clang")),
        b"",
    )
    .expect("clang marker");

    let output = Command::new(env!("CARGO_BIN_EXE_kira"))
        .env("KIRA_LLVM_HOME", &fake_llvm)
        .args(["build", "--backend", "llvm", "--sanitize", "address"])
        .arg(&source)
        .output()
        .expect("run kira build");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "a bundle with no compiler-rt built"
    );
    assert!(
        stderr.contains("the installed LLVM bundle has no Address Sanitizer runtime"),
        "{stderr}"
    );
    assert!(stderr.contains("update the pinned LLVM bundle"), "{stderr}");
    // Joined a segment at a time, the way the discovery that prints this
    // builds it. `join("lib/clang")` keeps the forward slash on Windows, so the
    // expected text would carry a separator the diagnostic never writes and the
    // match would fail there and nowhere else.
    assert!(
        stderr.contains(
            fake_llvm
                .join("lib")
                .join("clang")
                .to_string_lossy()
                .as_ref()
        ),
        "{stderr}"
    );

    let _ = std::fs::remove_dir_all(source.parent().expect("isolated source directory"));
}

/// The installed bundle really ships an Address Sanitizer runtime, and asking
/// for it really instruments the program.
///
/// The refusal above proves the *absence* is loud. Nothing proved the presence,
/// and a capability that is built, documented and never exercised is a
/// capability nobody knows is broken: this repository's own LLVM bundle for one
/// host turned out to hold objects for a different architecture, under the
/// right names, in archives that resolved every symbol a reader looked for.
///
/// So this asserts both halves on whichever host runs it. A bundle missing the
/// runtime fails here by name rather than being discovered the next time
/// somebody reaches for `--sanitize`, and a build that quietly produced an
/// uninstrumented binary — a sanitizer that reads as coverage and is not —
/// fails on the symbol count.
#[test]
fn the_installed_bundle_sanitizes_what_it_builds() {
    let source = write_isolated_source(PROGRAM);
    let output = Command::new(env!("CARGO_BIN_EXE_kira"))
        .args(["build", "--backend", "llvm", "--sanitize", "address"])
        .arg(&source)
        .output()
        .expect("run kira build");
    assert!(
        output.status.success(),
        "this host's LLVM bundle cannot build with `--sanitize address`: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let directory = source.parent().expect("isolated source directory");
    let executable = directory
        .join(".kira-build")
        .join(kira_toolchain::executable_name("main"));
    let bytes = std::fs::read(&executable).unwrap_or_else(|error| {
        panic!(
            "the sanitized build produced no `{}`: {error}",
            executable.display()
        )
    });
    // The runtime's own entry point, which every instrumented image carries and
    // no ordinary one does. Searched as bytes rather than through `nm`, which
    // is three different tools across these hosts and absent on one.
    assert!(
        contains(&bytes, b"__asan_init"),
        "`--sanitize address` produced a binary with no sanitizer runtime in it"
    );

    let _ = std::fs::remove_dir_all(directory);
}

/// Whether `haystack` contains `needle`.
fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

#[test]
fn sanitizer_is_refused_on_the_vm_and_web() {
    let source = write_isolated_source(PROGRAM);
    let vm = Command::new(env!("CARGO_BIN_EXE_kira"))
        .args(["build", "--backend", "vm", "--sanitize", "address"])
        .arg(&source)
        .output()
        .expect("run VM refusal");
    assert!(!vm.status.success());
    assert!(
        String::from_utf8_lossy(&vm.stderr).contains("the VM engine interprets"),
        "{}",
        String::from_utf8_lossy(&vm.stderr)
    );

    let web = Command::new(env!("CARGO_BIN_EXE_kira"))
        .args(["build", "--device", "wasm32", "--sanitize", "address"])
        .arg(&source)
        .output()
        .expect("run Web refusal");
    assert!(!web.status.success());
    assert!(
        String::from_utf8_lossy(&web.stderr).contains("the Web target emits WebAssembly"),
        "{}",
        String::from_utf8_lossy(&web.stderr)
    );

    let _ = std::fs::remove_dir_all(source.parent().expect("isolated source directory"));
}

#[test]
#[ignore = "needs an LLVM bundle built with compiler-rt"]
fn address_sanitizer_builds_a_native_program_with_the_pinned_bundle() {
    let source = write_isolated_source(PROGRAM);
    let output = Command::new(env!("CARGO_BIN_EXE_kira"))
        .args(["build", "--backend", "llvm", "--sanitize", "address"])
        .arg(&source)
        .output()
        .expect("run sanitized build");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let _ = std::fs::remove_dir_all(source.parent().expect("isolated source directory"));
}
