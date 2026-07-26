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

/// The host triples a declaration lists, all pointing at the one built archive
/// so the exact host this test runs on selects its own row.
const HOST_TRIPLES: [&str; 4] = [
    "aarch64-macos-none",
    "x86_64-macos-none",
    "x86_64-linux-gnu",
    "aarch64-linux-gnu",
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
    let dir =
        std::env::temp_dir().join(format!("kirac_ffi_inline_{}_{unique}", std::process::id()));
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
    Command::new(env!("CARGO_BIN_EXE_kirac"))
        .args(["run", "--backend", backend, entry.to_str().unwrap()])
        .output()
        .expect("run kirac")
}

#[test]
fn every_backend_agrees_on_the_ffi_fixture_and_shares_one_counter() {
    let entry = write_ffi_package(include_str!("../fixtures/ffi/ffi_program.kira"));

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

/// The same program, the same C archive, the same three backends — but the
/// library is declared inline in `package.kira` and there is no
/// `NativeLibs/*.toml` anywhere.
///
/// This is the corpus's own spelling (kira-graphics declares sokol this way and
/// ships no matching TOML), and until it resolved, no real app could link a
/// single `@FFI.Extern`. Byte-identical output to the file-declared run is the
/// statement: where a library is written changes nothing about what it does.
#[test]
fn a_library_declared_inline_in_the_package_links_on_every_backend() {
    let entry = write_inline_ffi_package(
        include_str!("../fixtures/ffi/ffi_program.kira"),
        ", systemLibs: [\"m\"]",
    );

    for backend in BACKENDS {
        let run = run_on(&entry, backend);
        assert_eq!(
            String::from_utf8_lossy(&run.stdout),
            EXPECTED,
            "the {backend} backend disagreed on the inline-declared FFI package\nstderr: {}",
            String::from_utf8_lossy(&run.stderr),
        );
        assert_eq!(
            run.status.code(),
            Some(0),
            "the {backend} backend did not exit cleanly\nstderr: {}",
            String::from_utf8_lossy(&run.stderr),
        );
    }

    let _ = std::fs::remove_dir_all(entry.parent().expect("package directory"));
}

/// A declared linker flag actually reaches the linker driver.
///
/// The positive test above cannot show this: `-lm` links whether or not it
/// arrives, so a row whose attributes were silently dropped would pass it. A
/// flag naming a library that does not exist can only be observed by failing
/// the link — so a clean exit here means the declaration never made it to the
/// command line.
#[test]
fn a_declared_linker_flag_reaches_the_link_line() {
    const ABSENT: &str = "kira_no_such_system_library";
    let entry = write_inline_ffi_package(
        include_str!("../fixtures/ffi/ffi_program.kira"),
        &format!(", linkerFlags: [\"-l{ABSENT}\"]"),
    );

    let run = run_on(&entry, "llvm");
    let stderr = String::from_utf8_lossy(&run.stderr);
    assert_ne!(
        run.status.code(),
        Some(0),
        "the link succeeded, so the declared linker flag never reached the driver\n\
         stdout: {}",
        String::from_utf8_lossy(&run.stdout),
    );
    assert!(
        stderr.contains(ABSENT),
        "the link failed for some other reason than the declared flag\nstderr: {stderr}",
    );

    let _ = std::fs::remove_dir_all(entry.parent().expect("package directory"));
}

/// C-layout structs cross the seam **by value**, in both directions, and every
/// backend has to agree on every byte.
///
/// The shapes are chosen for the ABI cases that a hand-written classifier gets
/// wrong, and that a `byval`/`sret` lowering cannot express at all:
///
/// - `ffi_quad` is four doubles — on AArch64 a homogeneous float aggregate
///   passed and returned in `v0`-`v3`, not in memory,
/// - `ffi_outer` nests a struct, so the Kira value has to be rebuilt with its
///   nesting rather than flattened,
/// - `ffi_mixed` pads between a `signed char` and a `double`, so a marshaller
///   that packed fields would produce a different number.
///
/// Kira classifies none of it: the generated C shim hands each struct to clang
/// by value, and clang applies the ABI it defines. What this test proves is that
/// the three engines then agree — and, because the values are computed by the C
/// side out of fields Kira wrote, that the bytes arrived where C expected them.
#[test]
fn every_backend_agrees_on_c_layout_structs_by_value() {
    let entry = write_ffi_package(include_str!("../fixtures/ffi/ffi_program_aggregate.kira"));

    // rect_sum(1.5, 2.5); rect_scale by 2; quad_sum(1+2+3+4); quad_make's first
    // and last; outer_sum(3+4+5) through a nested struct; outer_make's three
    // fields read back; mixed_sum(2+3+4) across padding; mixed_make's three.
    const EXPECTED_AGGREGATE: &str = "4\n3\n5\n10\n5\n8\n12\n11\n22\n33\n9\n7\n8\n9\n";

    for backend in BACKENDS {
        let run = run_on(&entry, backend);
        assert_eq!(
            String::from_utf8_lossy(&run.stdout),
            EXPECTED_AGGREGATE,
            "the {backend} backend disagreed on a struct crossing by value\nstderr: {}",
            String::from_utf8_lossy(&run.stderr),
        );
        assert_eq!(
            run.status.code(),
            Some(0),
            "the {backend} backend did not exit cleanly\nstderr: {}",
            String::from_utf8_lossy(&run.stderr),
        );
    }

    let _ = std::fs::remove_dir_all(entry.parent().expect("package directory"));
}

/// A single-scalar-field C handle struct crosses the seam as its field: the
/// result of `ffi_make_handle` (a `struct { unsigned int id; }` by value) is
/// rebuilt into the Kira `Handle`, and `ffi_handle_id` reads the field back out
/// of one. The round trip must produce the same `7 / 8 / 8` on every backend —
/// the LLVM/native and hybrid runs each actually call the C function whose ABI
/// prototype is the struct, through an adapter that names its single `U32`.
#[test]
fn every_backend_agrees_on_a_handle_struct_round_trip() {
    let entry = write_ffi_package(include_str!("../fixtures/ffi/ffi_program_handle.kira"));

    for backend in BACKENDS {
        let run = run_on(&entry, backend);
        assert_eq!(
            String::from_utf8_lossy(&run.stdout),
            "7\n8\n8\n",
            "the {backend} backend disagreed on the handle-struct round trip\nstderr: {}",
            String::from_utf8_lossy(&run.stderr),
        );
        assert_eq!(
            run.status.code(),
            Some(0),
            "the {backend} backend did not exit cleanly\nstderr: {}",
            String::from_utf8_lossy(&run.stderr),
        );
    }

    let _ = std::fs::remove_dir_all(entry.parent().expect("package directory"));
}
