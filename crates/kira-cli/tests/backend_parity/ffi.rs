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
        &format!(", systemLibs: [\"{HOST_SYSTEM_LIB}\"]"),
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

/// Inline `@FFI.Array` members cross by value, in both directions, on every
/// backend.
///
/// The two element shapes are covered: `ffi_grid` holds four `int`s inline
/// beside a `double`, and `ffi_board` holds three C structs inline beside an
/// `int`. Three writes prove the fill rule the two engines have to agree on —
/// a full array, a short one whose remaining C storage stays zero, and a
/// zero-filled construction with no elements at all — and the reads prove the
/// whole declared extent comes back, not a prefix.
#[test]
fn every_backend_agrees_on_inline_c_arrays() {
    let entry = write_ffi_package(include_str!("../fixtures/ffi/ffi_program_array.kira"));

    // gridSum full (20), short (11), zero-filled (3); gridMake(7)'s first and
    // last cells, its element count and weight; boardSum (27); boardMake(3)'s
    // third slot `p`, first slot `q`, and tag.
    const EXPECTED_ARRAY: &str = "20\n11\n3\n7\n10\n4\n70\n27\n5\n2\n300\n";

    for backend in BACKENDS {
        let run = run_on(&entry, backend);
        assert_eq!(
            String::from_utf8_lossy(&run.stdout),
            EXPECTED_ARRAY,
            "the {backend} backend disagreed on an inline C array\nstderr: {}",
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

/// A C function pointer crosses as a member and on its own, on every backend.
///
/// Three cases in one program: a zero-filled `@FFI.Callback` member is NULL and
/// C sees it as null; a pointer C handed out survives a round trip through a
/// Kira struct and is called back through it; and the same pointer passed on its
/// own reaches C as the pointer a function pointer is.
#[test]
fn every_backend_agrees_on_a_c_function_pointer() {
    let entry = write_ffi_package(include_str!("../fixtures/ffi/ffi_program_callback.kira"));

    // A null member (-1), (2 + 5) * 3, and 20 + 22.
    const EXPECTED_CALLBACK: &str = "-1\n21\n42\n";

    for backend in BACKENDS {
        let run = run_on(&entry, backend);
        assert_eq!(
            String::from_utf8_lossy(&run.stdout),
            EXPECTED_CALLBACK,
            "the {backend} backend disagreed on a C function pointer\nstderr: {}",
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

/// C calls a **Kira** function through a `@FFI.Callback`, on every backend.
///
/// The direction the seam was missing: the pointer is the address of a generated
/// entry thunk, and what it enters differs per backend. The VM's thunk reaches
/// the interpreter through the direct Libffi host, while a native build calls
/// the compiled function directly. C cannot tell, which is the point, so all
/// three must print the same numbers.
///
/// Three shapes: an immediate call back, two crossings in one call (so argument
/// order and a result feeding the next call are both observable), and a callback
/// C stores in a descriptor struct and calls after the call that gave it has
/// returned.
#[test]
fn every_backend_agrees_on_a_kira_function_called_from_c() {
    let entry = write_ffi_package(include_str!(
        "../fixtures/ffi/ffi_program_kira_callback.kira"
    ));

    // combine(3, 4) = 34; combine(combine(3, 4), 4) = 344; combine(5, 6) * 2.
    // Then the C-string callback: "kira" echoed and measured (4 * 100 + 7), and
    // a NULL argument arriving as the empty string (0 * 100 + 9).
    const EXPECTED_KIRA_CALLBACK: &str = "34\n344\n112\nkira!\n407\n!\n9\n";

    for backend in BACKENDS {
        let run = run_on(&entry, backend);
        assert_eq!(
            String::from_utf8_lossy(&run.stdout),
            EXPECTED_KIRA_CALLBACK,
            "the {backend} backend disagreed on a Kira function called from C\nstderr: {}",
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

/// C calls a Kira function with a **struct by value**, on every backend.
///
/// The direction Dawn forces. `wgpuInstanceRequestAdapter` is the only route to
/// an adapter, it answers through a callback, and that callback's third
/// parameter is a `WGPUStringView` by value — a field type fixed by the header,
/// with no userdata word and no sibling entry point to route around it.
///
/// Kira classifies none of it, exactly as it does not for a by-value argument:
/// the generated C entry takes the struct by value, so the target's own C
/// compiler decides how it arrives, and hands the entry thunk its address. The
/// Kira function declares the matching `@FFI.Pointer` and reads members through
/// it.
///
/// The two shapes are the ones a guessed classification gets wrong — a pointer
/// beside a length (`WGPUStringView` itself) and four doubles (an AArch64
/// homogeneous float aggregate in `v0`-`v3`) — with a scalar beside each so
/// argument order is observable, in both orders. The third line is the
/// asynchronous shape: C keeps the callback and enters it after the call that
/// gave it returned.
#[test]
fn every_backend_agrees_on_a_kira_callback_entered_with_a_struct() {
    let entry = write_ffi_package(include_str!(
        "../fixtures/ffi/ffi_program_struct_callback.kira"
    ));

    // 7 * 100 + 4; 3 * 100 + 0 through a NULL data pointer; 5 * 100 + 4 from
    // the stored callback; then a + d and b + c of the same four doubles.
    const EXPECTED_STRUCT_CALLBACK: &str = "704\n300\n504\n6.25\n5.75\n";

    for backend in BACKENDS {
        let run = run_on(&entry, backend);
        assert_eq!(
            String::from_utf8_lossy(&run.stdout),
            EXPECTED_STRUCT_CALLBACK,
            "the {backend} backend disagreed on a struct C passes to a Kira callback\nstderr: {}",
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

/// A `distinct` type crosses the seam as the scalar it is, on every backend.
///
/// The C function is `unsigned int ffi_id_u32(unsigned int)`, declared twice in
/// one program: once with `TabId` parameter and result, once with `U32`. Both
/// declarations bind the same symbol through the same `(u32) -> u32` wire
/// signature, so the two calls must print the same number — which is the ABI
/// transparency claim made where it can actually fail, in a real call to a real
/// C symbol on the native and hybrid engines rather than in an assertion about
/// a type table.
#[test]
fn every_backend_agrees_on_a_distinct_type_crossing_the_seam() {
    let entry = write_ffi_package(include_str!("../fixtures/ffi/ffi_program_distinct.kira"));

    for backend in BACKENDS {
        let run = run_on(&entry, backend);
        assert_eq!(
            String::from_utf8_lossy(&run.stdout),
            "4000000000\n4000000000\ntrue\n",
            "the {backend} backend disagreed on a distinct type at the C seam\nstderr: {}",
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

/// A `CString` member and a struct handed to C by address, on every backend.
///
/// The storage question both raise is the same one, and it has one answer: a
/// An ordinary descriptor call borrows its ownership tree and frees it on
/// return. `ffi_desc_keep` declares `retains: d`, so that one call consumes the
/// tree into the retained registry; `ffi_desc_recall` proves C can still read it
/// after the giving call returned.
///
/// The reference implementation crashes on the by-value case here. That is not
/// a behaviour to reproduce: the value is well defined, so this compiler
/// computes it rather than inheriting a segmentation fault.
#[test]
fn every_backend_agrees_on_a_c_string_member_and_a_struct_by_address() {
    let entry = write_ffi_package(include_str!("../fixtures/ffi/ffi_program_cstring.kira"));

    // A zero-filled title is NULL (0 * 10 + 7); "kira" by pointer (2 * 10 + 9)
    // and by value (2 * 10 + 8); the empty string is a real pointer, not NULL
    // (1 * 10 + 5); the kept pointer still reads "kira" (2 * 10 + 6); and a
    // retained by-value member still reads it too (2 * 10 + 4); and a retained
    // top-level CString survives as well (2 * 10 + 3).
    const EXPECTED_CSTRING: &str = "7\n29\n28\n15\n26\n24\n23\n";

    for backend in BACKENDS {
        let run = run_on(&entry, backend);
        assert_eq!(
            String::from_utf8_lossy(&run.stdout),
            EXPECTED_CSTRING,
            "the {backend} backend disagreed on a CString member\nstderr: {}",
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

#[test]
fn retained_c_storage_is_counted_and_everything_else_balances() {
    let entry = write_ffi_package(include_str!("../fixtures/ffi/ffi_program_cstring.kira"));
    for backend in ["llvm", "hybrid"] {
        let run = run_on_with_heap_report(&entry, backend);
        let report = assert_native_heap_balanced(backend, &run);
        assert_eq!(
            report.retained,
            4,
            "retained pointer, by-value, and CString calls own four blocks on {backend}\n{}",
            String::from_utf8_lossy(&run.stderr),
        );
    }
    let _ = std::fs::remove_dir_all(entry.parent().expect("package directory"));
}

/// Members read *through* an `@FFI.Pointer`, on every backend.
///
/// C owns the struct and hands over its address; Kira reads the members behind
/// it with no accessor compiled into a shim. That is what the twenty
/// `kg_event_*` accessors in kira-graphics existed to work around.
///
/// The members are of mixed width and signedness with padding between them, so
/// a read at the wrong offset gives a wrong *answer* rather than a crash — the
/// failure a backend can have silently. `next` is read through and then read
/// through again, which is both the linked-structure shape and the proof that a
/// member's offset is its own rather than its first leaf's.
#[test]
fn every_backend_agrees_on_members_read_through_a_pointer() {
    let entry = write_ffi_package(include_str!(
        "../fixtures/ffi/ffi_program_pointer_read.kira"
    ));

    // `kind` is 200, which is only positive read as unsigned; `code` is -1234,
    // which is only negative read as signed; `delta` is -7 from a `signed char`;
    // `weight` is 1.5 widened from `float`; the tail's `kind` is 9; and the
    // first and last touches' `pos_y` are 20.5 and 80.5, which come out right
    // only if the array member's own offset and the element stride are both
    // right.
    const EXPECTED_POINTER_READ: &str = "200
-1234
-7
1.5
9
20.5
80.5
";

    for backend in BACKENDS {
        let run = run_on(&entry, backend);
        assert_eq!(
            String::from_utf8_lossy(&run.stdout),
            EXPECTED_POINTER_READ,
            "the {backend} backend disagreed on a member read through a pointer
stderr: {}",
            String::from_utf8_lossy(&run.stderr),
        );
        assert_eq!(
            run.status.code(),
            Some(0),
            "the {backend} backend did not exit cleanly
stderr: {}",
            String::from_utf8_lossy(&run.stderr),
        );
    }
}

/// A Kira array handed to C as a pointer and a count, on every backend.
///
/// This is the shape every graphics API takes, and without it a caller streams
/// values into a C-side buffer one at a time through a shim — which is what
/// kira-graphics did before this.
///
/// The widths differ on purpose. Kira holds a `[F32]` as `double`s and C reads
/// four bytes each, so a seam that handed over the array's own storage would
/// give C wrong *numbers* rather than a wrong pointer — the kind of failure that
/// looks like a rendering bug rather than a crash.
#[test]
fn every_backend_agrees_on_an_array_handed_to_c() {
    let entry = write_ffi_package(include_str!(
        "../fixtures/ffi/ffi_program_array_argument.kira"
    ));

    // 1.5 + 2.25 + 3.0, then 10 + 20 + 30 + 40, then an empty array — which is
    // a null pointer and a zero count rather than a trap.
    const EXPECTED_ARRAY_ARGUMENT: &str = "6.75\n100\n0\n";

    for backend in BACKENDS {
        let run = run_on(&entry, backend);
        assert_eq!(
            String::from_utf8_lossy(&run.stdout),
            EXPECTED_ARRAY_ARGUMENT,
            "the {backend} backend disagreed on an array handed to C\nstderr: {}",
            String::from_utf8_lossy(&run.stderr),
        );
        assert_eq!(
            run.status.code(),
            Some(0),
            "the {backend} backend did not exit cleanly\nstderr: {}",
            String::from_utf8_lossy(&run.stderr),
        );
    }
}

/// The same array, named as a C-layout struct's data pointer, on every backend.
///
/// A graphics API rarely takes a pointer and a count as two arguments: it takes
/// a descriptor holding both. `sg_range` is the one every sokol upload goes
/// through, and while an array could only cross at an argument, that descriptor
/// had to be built in a C helper whose whole job was naming the address of an
/// array — two of them survived in kira-graphics for exactly that reason.
///
/// The last line is the one that matters most: C keeps the pointer and reads it
/// after the call returns, so it fails if the elements got storage that dies
/// with the descriptor naming them.
#[test]
fn every_backend_agrees_on_an_array_named_in_a_c_layout_member() {
    let entry = write_ffi_package(include_str!(
        "../fixtures/ffi/ffi_program_array_member.kira"
    ));

    // 1.5 + 2.25 + 3.0 by address, 10 + 20 + 30 + 40 by value, an empty array —
    // which is a null pointer the fixture answers -1 for — then 7 + 8 + 9 read
    // back out of a pointer C kept.
    const EXPECTED_ARRAY_MEMBER: &str = "6.75\n100\n-1\n24\n";

    for backend in BACKENDS {
        let run = run_on(&entry, backend);
        assert_eq!(
            String::from_utf8_lossy(&run.stdout),
            EXPECTED_ARRAY_MEMBER,
            "the {backend} backend disagreed on an array named in a C-layout member\nstderr: {}",
            String::from_utf8_lossy(&run.stderr),
        );
        assert_eq!(
            run.status.code(),
            Some(0),
            "the {backend} backend did not exit cleanly\nstderr: {}",
            String::from_utf8_lossy(&run.stderr),
        );
    }
}

/// An item list — several C structs behind one pointer — on every backend.
///
/// A pointer position already took the struct it points at, which covers a
/// descriptor with one of something. Every descriptor-driven graphics API asks
/// for several beside a count instead: vertex attributes, bind group entries,
/// colour targets. A C array is its elements laid out end to end, which is what
/// an `@FFI.Array` reserves, so the array type fills the pointer.
///
/// The third line is the one worth watching: a Kira array shorter than the
/// extent zero-fills the rest, and the count beside the pointer is what says
/// how many C reads.
#[test]
fn every_backend_agrees_on_an_item_list_behind_one_pointer() {
    let entry = write_ffi_package(include_str!("../fixtures/ffi/ffi_program_item_list.kira"));

    // Three items by argument, one by argument, two of a four-slot extent, and
    // two named inside a descriptor whose member is an `@FFI.Pointer`.
    const EXPECTED_ITEM_LIST: &str = "6040\n4005\n2003\n12014\n";

    for backend in BACKENDS {
        let run = run_on(&entry, backend);
        assert_eq!(
            String::from_utf8_lossy(&run.stdout),
            EXPECTED_ITEM_LIST,
            "the {backend} backend disagreed on an item list behind one pointer\nstderr: {}",
            String::from_utf8_lossy(&run.stderr),
        );
        assert_eq!(
            run.status.code(),
            Some(0),
            "the {backend} backend did not exit cleanly\nstderr: {}",
            String::from_utf8_lossy(&run.stderr),
        );
    }
}

/// An extension chained onto a descriptor, on every backend.
///
/// The pointer member's target is the base link; the value written is a struct
/// that *begins* with one. A struct and its first member share an address, which
/// is what every `nextInChain`/`pNext` cast in an extensible header relies on —
/// and reaching a WebGPU surface from a window has no other shape at all.
#[test]
fn every_backend_agrees_on_an_extension_chained_onto_a_descriptor() {
    let entry = write_ffi_package(include_str!("../fixtures/ffi/ffi_program_chained.kira"));

    // 7 with no chain, 7*3 with one link, 7*3*5 with two, and 7 again for a
    // link the walker does not recognise.
    const EXPECTED_CHAINED: &str = "7\n21\n105\n7\n";

    for backend in BACKENDS {
        let run = run_on(&entry, backend);
        assert_eq!(
            String::from_utf8_lossy(&run.stdout),
            EXPECTED_CHAINED,
            "the {backend} backend disagreed on a chained extension\nstderr: {}",
            String::from_utf8_lossy(&run.stderr),
        );
        assert_eq!(
            run.status.code(),
            Some(0),
            "the {backend} backend did not exit cleanly\nstderr: {}",
            String::from_utf8_lossy(&run.stderr),
        );
    }
}

/// A Kira enum and an array named directly in a foreign signature.
///
/// The enum crosses as its case's number, which is what a C enum is; the array
/// crosses as a pointer to elements the seam writes out in C's widths.
#[test]
fn every_backend_agrees_on_enums_and_arrays_named_in_a_signature() {
    let entry = write_ffi_package(include_str!(
        "../fixtures/ffi/ffi_program_named_shapes.kira"
    ));

    // The three strides the C enum switches on, the float sum, and the struct.
    const EXPECTED_NAMED_SHAPES: &str = "24\n4\n16\n6.75\n42\n";

    for backend in BACKENDS {
        let run = run_on(&entry, backend);
        assert_eq!(
            String::from_utf8_lossy(&run.stdout),
            EXPECTED_NAMED_SHAPES,
            "the {backend} backend disagreed on a named shape\nstderr: {}",
            String::from_utf8_lossy(&run.stderr),
        );
        assert_eq!(
            run.status.code(),
            Some(0),
            "the {backend} backend did not exit cleanly\nstderr: {}",
            String::from_utf8_lossy(&run.stderr),
        );
    }
}

/// An array argument travels *out* only: C reads a copy, and writes to it are
/// not visible afterwards.
///
/// The seam writes the elements into its own buffer in C's widths, because Kira
/// holds them in Kira's. Handing over the array's own storage is not an option,
/// so a write through the pointer lands somewhere Kira does not read — which is
/// worth pinning, because a caller reaching for the out-parameter shape a C API
/// often uses would otherwise find it silently doing nothing.
#[test]
fn an_array_argument_is_a_copy_out_not_a_shared_buffer() {
    let entry = write_ffi_package(
        r#"
@FFI.Extern { library: ffifixture, symbol: ffi_fill_floats, abi: c }
function ffiFillFloats(values: [F32], count: I32): Void

@Main
function main() {
    var values: [F32] = [1.0, 2.0]
    ffiFillFloats(values, 2)
    print(values[0])
    print(values[1])
    return
}
"#,
    );

    // 1 and 2, not the 99s C wrote into the buffer it was handed.
    for backend in BACKENDS {
        let run = run_on(&entry, backend);
        assert_eq!(
            String::from_utf8_lossy(&run.stdout),
            "1\n2\n",
            "the {backend} backend disagreed on an array argument's direction\nstderr: {}",
            String::from_utf8_lossy(&run.stderr),
        );
    }
}
