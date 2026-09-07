//! Seamless C-FFI parity: one package calling real C symbols must produce
//! byte-identical output and exit status on the VM, LLVM/native, and hybrid
//! backends.
//!
//! This file is the shared harness — the C fixture's build, the two package
//! spellings, and the one runner — and the cases live beside it, grouped by
//! what crosses: [`library`] the whole declared surface and how it links,
//! [`scalars`] the exact C width and value of each seam scalar, [`aggregates`]
//! C-layout structs and their members, [`callbacks`] function pointers in both
//! directions, and [`pointers`] the pointer word itself.
//!
//! The C fixture is compiled here with the managed clang and llvm-ar, so the
//! test needs no checked-in binary and no system headers.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU32, Ordering};

use crate::{assert_native_heap_balanced, run_on_with_heap_report};

/// The output every backend must produce, line for line.
const EXPECTED: &str = "42\n-5\n200\n-9\n40000\n4000000000\n1975\n5000000000\nfalse\n3.75\n1.75\n\
     4\n42\n0\n7\nhello from C\nround trip\n|\nhello from C!\n1\n2\n";

/// Every backend the FFI program must behave identically on.
const BACKENDS: [&str; 3] = ["vm", "llvm", "hybrid"];

/// A system library every host has, spelled the way that host spells it.
///
/// `m` is the Unix math library. Windows has no `m.lib` — its math lives in the
/// UCRT, which every link already gets — so naming `m` there fails the link with
/// `LNK1181` rather than proving anything about declared system libraries.
const HOST_SYSTEM_LIB: &str = if cfg!(target_env = "msvc") {
    "kernel32"
} else {
    "m"
};

/// The host triples a declaration lists, all pointing at the one built archive
/// so the exact host this test runs on selects its own row.
const HOST_TRIPLES: [&str; 6] = [
    "aarch64-macos-none",
    "x86_64-macos-none",
    "x86_64-linux-gnu",
    "aarch64-linux-gnu",
    "x86_64-windows-msvc",
    "aarch64-windows-msvc",
];

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
fn write_ffi_package(program: &str) -> PathBuf {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("kira_ffi_{}_{unique}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");

    build_fixture_archive(&dir);
    std::fs::write(
        dir.join("package.kira"),
        "Package FfiParity {\n    let allowThinFfiShim = true\n}\n",
    )
    .expect("package manifest");
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
[[target]]
triple = "x86_64-windows-msvc"
staticLib = "lib/libffifixture.a"
[[target]]
triple = "aarch64-windows-msvc"
staticLib = "lib/libffifixture.a"
"#,
    )
    .expect("native-lib manifest");

    let entry = dir.join("main.kira");
    std::fs::write(&entry, program).expect("program");
    entry
}

/// Writes the same FFI package declaring its library **inline in
/// `package.kira`**, with no `NativeLibs/*.toml` at all.
///
/// This is the other of the two declaration spellings, and the one the corpus
/// actually uses for sokol. Inline paths anchor at the package root rather than
/// at a TOML's own directory, so the rows name `NativeLibs/lib/...` where the
/// file spelling names `lib/...`.
///
/// `extra_fields` is appended to every target row, which is how the tests below
/// make a declared link attribute observable.
fn write_inline_ffi_package(program: &str, extra_fields: &str) -> PathBuf {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("kira_ffi_inline_{}_{unique}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");

    build_fixture_archive(&dir);
    let rows: String = HOST_TRIPLES
        .iter()
        .map(|triple| {
            format!(
                "                NativeTarget {{ triple: \"{triple}\", \
                 staticLib: \"NativeLibs/lib/libffifixture.a\"{extra_fields} }}"
            )
        })
        .collect::<Vec<_>>()
        .join(",\n");
    std::fs::write(
        dir.join("package.kira"),
        format!(
            "Package FfiInline {{\n\
             \x20   let version = \"0.1.0\"\n\
             \x20   let kind = .App\n\
             \x20   let allowThinFfiShim = true\n\
             \x20   let nativeLibraries = [\n\
             \x20       NativeLibrary {{\n\
             \x20           name: \"ffifixture\",\n\
             \x20           linkMode: LinkMode.Static,\n\
             \x20           nativeTargets: [\n{rows}\n            ],\n\
             \x20       }}\n\
             \x20   ]\n\
             }}\n"
        ),
    )
    .expect("package declaration");

    let entry = dir.join("main.kira");
    std::fs::write(&entry, program).expect("program");
    entry
}

/// Runs the FFI program on one backend.
fn run_on(entry: &Path, backend: &str) -> Output {
    Command::new(env!("CARGO_BIN_EXE_kira"))
        .args(["run", "--backend", backend, entry.to_str().unwrap()])
        .output()
        .expect("run kira")
}

mod aggregates;
mod callbacks;
mod library;
mod pointers;
mod scalars;
