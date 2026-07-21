//! Portable C-FFI on wasm: the emscripten C archive and its generated adapters
//! are linked into the module and the program runs under node, and a package
//! whose library has no wasm artifact is refused before `emcc` is ever invoked.
//!
//! Requires `emcc`, `emar`, and `node` on PATH, exactly as building for the Web
//! does; a machine without them fails here rather than skipping, so a green run
//! means the whole path was exercised.
//!
//! The wasm program is the scalar subset of the fixture — every supported type
//! except `CString`. `CString` needs a Kira string, and Kira string creation on
//! wasm32 is blocked by a pre-existing width mismatch in `kira_rt_str_new` that
//! is independent of FFI (any wasm program printing a string literal hits it).
//! `CString` length is proven on the host backends, where string creation is
//! sound; see `docs/ffi.md`.

use std::path::Path;
use std::process::Command;

use crate::ffi::{HOST_MANIFEST, build_host_archive, scratch};

/// The scalar subset's output, line for line.
const EXPECTED: &str =
    "42\n-5\n200\n-9\n40000\n4000000000\n1975\n5000000000\nfalse\n3.75\n1.75\n42\n0\n7\n";

/// Compiles the checked-in C fixture into an emscripten static archive under
/// `dir` with `emcc` and `emar`.
fn build_wasm_archive(dir: &Path) {
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
    let compile = Command::new("emcc")
        .arg("-c")
        .arg(&source)
        .arg("-o")
        .arg(&object)
        .arg("-I")
        .arg(&lib)
        .output()
        .expect("emcc runs (required for the wasm FFI test)");
    assert!(
        compile.status.success(),
        "compiling the C fixture with emcc failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let archive = lib.join("libffifixture-wasm.a");
    let ar = Command::new("emar")
        .arg("crs")
        .arg(&archive)
        .arg(&object)
        .output()
        .expect("emar runs (required for the wasm FFI test)");
    assert!(
        ar.status.success(),
        "archiving the C fixture with emar failed: {}",
        String::from_utf8_lossy(&ar.stderr)
    );
}

#[test]
fn a_wasm_build_links_the_emscripten_archive_and_runs_under_node() {
    let dir = scratch("wasm");
    build_wasm_archive(&dir);
    std::fs::write(
        dir.join("NativeLibs/ffifixture.toml"),
        "name = \"ffifixture\"\n\
         [[target]]\n\
         triple = \"wasm32-emscripten-unknown\"\n\
         staticLib = \"lib/libffifixture-wasm.a\"\n",
    )
    .expect("manifest");
    let entry = dir.join("main.kira");
    std::fs::write(
        &entry,
        include_str!("../fixtures/ffi/ffi_program_scalar.kira"),
    )
    .expect("program");

    let build = Command::new(env!("CARGO_BIN_EXE_kirac"))
        .args(["build", "--device", "wasm32", entry.to_str().unwrap()])
        .output()
        .expect("run kirac");
    assert!(
        build.status.success(),
        "the wasm build failed\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&build.stdout),
        String::from_utf8_lossy(&build.stderr),
    );

    let js = dir.join(".kira-build/web/main.js");
    let node = Command::new("node")
        .arg(&js)
        .output()
        .expect("run node (required for the wasm FFI test)");
    assert!(
        node.status.success(),
        "the wasm module must run to completion under node: {}",
        String::from_utf8_lossy(&node.stderr),
    );
    assert_eq!(
        String::from_utf8_lossy(&node.stdout),
        EXPECTED,
        "the wasm FFI program produced unexpected output",
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_host_only_library_is_refused_before_emcc() {
    // The package declares its library only for host triples. Asking for wasm is
    // a clean structural miss the resolver names before any code generation, so
    // `emcc` is never invoked.
    let dir = scratch("wasm-hostonly");
    build_host_archive(&dir);
    std::fs::write(dir.join("NativeLibs/ffifixture.toml"), HOST_MANIFEST).expect("manifest");
    let entry = dir.join("main.kira");
    std::fs::write(
        &entry,
        include_str!("../fixtures/ffi/ffi_program_scalar.kira"),
    )
    .expect("program");

    let build = Command::new(env!("CARGO_BIN_EXE_kirac"))
        .args(["build", "--device", "wasm32", entry.to_str().unwrap()])
        .output()
        .expect("run kirac");
    assert_eq!(
        build.status.code(),
        Some(1),
        "a host-only library asked for wasm must fail",
    );
    let stderr = String::from_utf8_lossy(&build.stderr);
    assert!(
        stderr.contains("ffifixture") && stderr.contains("wasm32-emscripten-unknown"),
        "the diagnostic must name the library and the wasm target: {stderr}",
    );
    let _ = std::fs::remove_dir_all(&dir);
}
