//! Seamless C-FFI parity: one package calling real C symbols must produce
//! byte-identical output and exit status on the VM, LLVM/native, and hybrid
//! backends.
//!
//! The final two lines — `1` then `2` — are the hybrid single-copy proof: one
//! wrapper is `@Runtime` and the other `@Native`, so on the hybrid backend the
//! first foreign call runs through the VM half's `call_foreign` and the second
//! runs as machine code, and both must observe the same C counter. On the VM and
//! LLVM backends the annotations do nothing, so the same two lines fall out of a
//! single engine — which is exactly the parity statement.
//!
//! The C fixture is compiled here with the managed clang and llvm-ar, so the
//! test needs no checked-in binary and no system headers.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU32, Ordering};

/// The output every backend must produce, line for line.
const EXPECTED: &str =
    "42\n-5\n200\n-9\n40000\n4000000000\n1975\n5000000000\nfalse\n3.75\n1.75\n4\n42\n0\n7\n1\n2\n";

/// Every backend the FFI program must behave identically on.
const BACKENDS: [&str; 3] = ["vm", "llvm", "hybrid"];

/// Compiles the checked-in C fixture into `NativeLibs/lib/libffifixture.a` under
/// `dir`, using the managed clang and llvm-ar.
fn build_fixture_archive(dir: &Path) -> PathBuf {
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
        .arg("-c")
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
    archive
}

/// Writes the FFI package into a fresh temp directory and returns its entry.
///
/// A bare `.kira` entry with a `NativeLibs/` beside it, so the package root the
/// CLI resolves archives against is the entry's own directory. The manifest
/// lists several host triples pointing at the one built archive, so the exact
/// host the test runs on selects its own row.
fn write_ffi_package() -> PathBuf {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("kirac_ffi_{}_{unique}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");

    build_fixture_archive(&dir);
    std::fs::write(
        dir.join("NativeLibs/ffifixture.toml"),
        r#"name = "ffifixture"
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
"#,
    )
    .expect("native-lib manifest");

    let entry = dir.join("main.kira");
    std::fs::write(&entry, include_str!("../fixtures/ffi/ffi_program.kira")).expect("program");
    entry
}

/// Runs the FFI program on one backend.
fn run_on(entry: &Path, backend: &str) -> Output {
    Command::new(env!("CARGO_BIN_EXE_kirac"))
        .args(["run", "--backend", backend, entry.to_str().unwrap()])
        .output()
        .expect("run kirac")
}

#[test]
fn every_backend_agrees_on_the_ffi_fixture_and_shares_one_counter() {
    let entry = write_ffi_package();

    let runs: Vec<(&str, Output)> = BACKENDS
        .iter()
        .map(|backend| (*backend, run_on(&entry, backend)))
        .collect();

    for (backend, run) in &runs {
        let stdout = String::from_utf8_lossy(&run.stdout);
        assert_eq!(
            stdout,
            EXPECTED,
            "the {backend} backend produced unexpected FFI output\nstderr: {}",
            String::from_utf8_lossy(&run.stderr),
        );
        assert_eq!(
            run.status.code(),
            Some(0),
            "the {backend} backend did not exit cleanly\nstderr: {}",
            String::from_utf8_lossy(&run.stderr),
        );
    }

    // The counter lines are the single-copy proof: whichever way the two foreign
    // calls were routed, they counted 1 then 2 rather than 1 then 1.
    for (backend, run) in &runs {
        let stdout = String::from_utf8_lossy(&run.stdout);
        let tail: Vec<&str> = stdout.lines().rev().take(2).collect();
        assert_eq!(
            tail,
            ["2", "1"],
            "the {backend} backend's counter did not advance 1 then 2",
        );
    }

    let _ = std::fs::remove_dir_all(entry.parent().expect("package directory"));
}
