//! End-to-end C-FFI on the built `kirac`: a real VM foreign call through a
//! generated adapter sidecar, and the typed diagnostics a misdeclared package
//! gets.
//!
//! The sidecar call is the one that proves the VM path is not a smoke surface:
//! `kirac run --backend vm` builds a foreign-adapter sidecar, loads it through a
//! native-capable host, and answers `call_foreign` out of it — the VM itself
//! still links and `dlopen`s nothing. The output is Kira-produced, computed by
//! real C symbols reached through the generated adapters.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU32, Ordering};

/// The output the full fixture prints on the VM, through the sidecar.
pub(crate) const EXPECTED: &str = "42\n-5\n200\n-9\n40000\n4000000000\n1975\n5000000000\nfalse\n3.75\n1.75\n\
     4\n42\n0\n7\nhello from C\nround trip\n|\nhello from C!\n1\n2\n";

/// A fresh temp directory unique to one test.
pub(crate) fn scratch(tag: &str) -> PathBuf {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "kirac_e2e_ffi_{tag}_{}_{unique}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir
}

/// Compiles the checked-in C fixture into `NativeLibs/lib/libffifixture.a` under
/// `dir` with the managed clang and llvm-ar.
pub(crate) fn build_host_archive(dir: &Path) {
    let llvm = kira_toolchain::discover(None).expect("the managed LLVM is present");
    let lib = dir.join("NativeLibs/lib");
    std::fs::create_dir_all(&lib).expect("native-lib directory");

    let source = lib.join("ffi_fixture.c");
    std::fs::write(&source, include_str!("../fixtures/ffi/ffi_fixture.c")).expect("fixture source");
    std::fs::write(
        lib.join("ffi_fixture.h"),
        include_str!("../fixtures/ffi/ffi_fixture.h"),
    )
    .expect("fixture header");

    let object = lib.join("ffi_fixture.o");
    let compile = Command::new(llvm.clang())
        .args(["-c"])
        .arg(&source)
        .arg("-o")
        .arg(&object)
        .arg("-I")
        .arg(&lib)
        .output()
        .expect("clang runs");
    assert!(
        compile.status.success(),
        "compiling the C fixture failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let archive = lib.join("libffifixture.a");
    let ar = Command::new(llvm.llvm_ar())
        .arg("crs")
        .arg(&archive)
        .arg(&object)
        .output()
        .expect("llvm-ar runs");
    assert!(
        ar.status.success(),
        "archiving the C fixture failed: {}",
        String::from_utf8_lossy(&ar.stderr)
    );
}

/// The host `NativeLibs/*.toml` covering the common host triples, each pointing
/// at the one built archive.
pub(crate) const HOST_MANIFEST: &str = r#"name = "ffifixture"
[[target]]
triple = "aarch64-macos-none"
staticLib = "lib/libffifixture.a"
[[target]]
triple = "x86_64-macos-none"
staticLib = "lib/libffifixture.a"
[[target]]
triple = "x86_64-linux-gnu"
staticLib = "lib/libffifixture.a"
[[target]]
triple = "aarch64-linux-gnu"
staticLib = "lib/libffifixture.a"
"#;

/// Runs `kirac run` with `args` and returns its output.
fn run(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_kirac"))
        .args(args)
        .output()
        .expect("run kirac")
}

#[test]
fn a_vm_run_calls_c_through_a_generated_adapter_sidecar() {
    let dir = scratch("sidecar");
    build_host_archive(&dir);
    std::fs::write(dir.join("NativeLibs/ffifixture.toml"), HOST_MANIFEST).expect("manifest");
    let entry = dir.join("main.kira");
    std::fs::write(&entry, include_str!("../fixtures/ffi/ffi_program.kira")).expect("program");

    let output = run(&["run", "--backend", "vm", entry.to_str().unwrap()]);
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        EXPECTED,
        "the VM sidecar run produced unexpected output\nstderr: {}",
        String::from_utf8_lossy(&output.stderr),
    );
    assert_eq!(output.status.code(), Some(0));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_undeclared_native_library_is_a_typed_diagnostic() {
    // The program declares `@FFI.Extern` imports naming `ffifixture`, but the
    // package ships no `NativeLibs` at all: the resolver refuses before any code
    // generation, naming the library and the target.
    let dir = scratch("undeclared");
    let entry = dir.join("main.kira");
    std::fs::write(&entry, include_str!("../fixtures/ffi/ffi_program.kira")).expect("program");

    let output = run(&["run", "--backend", "vm", entry.to_str().unwrap()]);
    assert_eq!(
        output.status.code(),
        Some(1),
        "an undeclared library must fail"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("ffifixture") && stderr.contains("does not declare"),
        "the diagnostic must name the undeclared library: {stderr}",
    );
    assert!(
        output.stdout.is_empty(),
        "a refused build must print nothing to stdout",
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_seam_accepts_a_cstring_result_and_still_refuses_a_string() {
    // Both halves of one rule, proven through the CLI: `CString` is the seam's
    // spelling for C text in either direction, and `String` is Kira's — naming
    // Kira's at the seam is still the mistake it always was, and the refusal
    // reaches the CLI as a rendered diagnostic with its stable code.
    let dir = scratch("cstring-result");

    let good = dir.join("good.kira");
    std::fs::write(
        &good,
        "@FFI.Extern { library: l; symbol: s; abi: c; }\n\
         function greeting() -> CString;\n\
         @Main function main() { print(greeting()) return }\n",
    )
    .expect("program");
    let output = run(&["check", good.to_str().unwrap()]);
    assert_eq!(
        output.status.code(),
        Some(0),
        "a CString result checks clean: {}",
        String::from_utf8_lossy(&output.stderr),
    );

    let bad = dir.join("bad.kira");
    std::fs::write(
        &bad,
        "@FFI.Extern { library: l; symbol: s; abi: c; }\n\
         function bad() -> String;\n\
         @Main function main() { return }\n",
    )
    .expect("program");
    let output = run(&["check", bad.to_str().unwrap()]);
    assert_eq!(output.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("KSEM182"),
        "a String result must be refused with KSEM182: {}",
        String::from_utf8_lossy(&output.stderr),
    );
    let _ = std::fs::remove_dir_all(&dir);
}
