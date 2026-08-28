//! Portable C-FFI on wasm: the emscripten C archive and its generated adapters
//! are linked into the module and the program runs under node, and a package
//! whose library has no wasm artifact is refused before `emcc` is ever invoked.
//!
//! Requires `emcc`, `emar`, and `node` on PATH, exactly as building for the Web
//! does; a machine without them fails here rather than skipping, so a green run
//! means the whole path was exercised.
//!
//! The fixture is exercised by scalar calls, string operations, direct scalar
//! and `CString` callbacks, and a by-value aggregate callback. Together they
//! prove that the generated C and LLVM paths use the target's ABI and pointer
//! width rather than the host's.

use std::path::Path;
use std::process::Command;

use crate::ffi::{HOST_MANIFEST, build_host_archive, scratch};

/// The scalar subset's output, line for line.
const EXPECTED: &str =
    "42\n-5\n200\n-9\n40000\n4000000000\n1975\n5000000000\nfalse\n3.75\n1.75\n42\n0\n7\n";

/// The string program's output, line for line.
const EXPECTED_STRINGS: &str = "catdog\n6\n99\natd\n3\n42!\n5\n6\nhello from C\nround trip\n";

/// The scalar and `CString` callback program's output, line for line.
const EXPECTED_CALLBACK: &str = "34\n344\n112\nkira!\n407\n!\n9\n";

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

    let build = Command::new(env!("CARGO_BIN_EXE_kira"))
        .args(["build", "--device", "wasm32", entry.to_str().unwrap()])
        .output()
        .expect("run kira");
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

/// Kira strings, the string primitives, and `CString` in both directions, run
/// as wasm under node.
///
/// This is the case a wasm module could not do at all while the length passed
/// to `kira_rt_str_new` was emitted at the host's pointer width: the emscripten
/// archive expects a 32-bit `usize`, the mismatch resolved by name, and every
/// string trapped.
#[test]
fn a_wasm_build_creates_kira_strings_and_crosses_the_cstring_seam() {
    let dir = scratch("wasm-strings");
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
        include_str!("../fixtures/ffi/ffi_program_wasm_string.kira"),
    )
    .expect("program");

    let build = Command::new(env!("CARGO_BIN_EXE_kira"))
        .args(["build", "--device", "wasm32", entry.to_str().unwrap()])
        .output()
        .expect("run kira");
    assert!(
        build.status.success(),
        "the wasm string build failed\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&build.stdout),
        String::from_utf8_lossy(&build.stderr),
    );

    let node = Command::new("node")
        .arg(dir.join(".kira-build/web/main.js"))
        .output()
        .expect("run node (required for the wasm FFI test)");
    assert!(
        node.status.success(),
        "the wasm module must run to completion under node: {}",
        String::from_utf8_lossy(&node.stderr),
    );
    assert_eq!(
        String::from_utf8_lossy(&node.stdout),
        EXPECTED_STRINGS,
        "the wasm string program produced unexpected output",
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// Scalar and `CString` callbacks use direct LLVM functions in a Web module;
/// unlike the host path they do not require a libffi closure to manufacture
/// their address.
#[test]
fn a_wasm_build_enters_scalar_and_string_callbacks_directly() {
    let dir = scratch("wasm-kira-callback");
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
        include_str!("../fixtures/ffi/ffi_program_kira_callback.kira"),
    )
    .expect("program");

    let build = Command::new(env!("CARGO_BIN_EXE_kira"))
        .args(["build", "--device", "wasm32", entry.to_str().unwrap()])
        .output()
        .expect("run kira");
    assert!(
        build.status.success(),
        "the wasm callback build failed\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&build.stdout),
        String::from_utf8_lossy(&build.stderr),
    );

    let node = Command::new("node")
        .arg(dir.join(".kira-build/web/main.js"))
        .output()
        .expect("run node (required for the wasm FFI test)");
    assert!(
        node.status.success(),
        "the wasm callback module must run to completion under node: {}",
        String::from_utf8_lossy(&node.stderr),
    );
    assert_eq!(
        String::from_utf8_lossy(&node.stdout),
        EXPECTED_CALLBACK,
        "the wasm callback program produced unexpected output",
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A Web callback whose C signature contains a by-value aggregate uses the
/// generated C entry rather than the host-only libffi closure path. The entry
/// lets emscripten classify the struct, then forwards its address to the LLVM
/// callback body; the body reads the same fields the native callback does.
#[test]
fn a_wasm_build_enters_a_struct_callback_through_its_generated_c_entry() {
    let dir = scratch("wasm-struct-callback");
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
        include_str!("../fixtures/ffi/ffi_program_struct_callback.kira"),
    )
    .expect("program");

    let build = Command::new(env!("CARGO_BIN_EXE_kira"))
        .args(["build", "--device", "wasm32", entry.to_str().unwrap()])
        .output()
        .expect("run kira");
    assert!(
        build.status.success(),
        "the wasm struct-callback build failed\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&build.stdout),
        String::from_utf8_lossy(&build.stderr),
    );

    let node = Command::new("node")
        .arg(dir.join(".kira-build/web/main.js"))
        .output()
        .expect("run node (required for the wasm FFI test)");
    assert!(
        node.status.success(),
        "the wasm struct callback must run to completion under node: {}",
        String::from_utf8_lossy(&node.stderr),
    );
    assert_eq!(
        String::from_utf8_lossy(&node.stdout),
        "704\n300\n504\n6.25\n5.75\n",
        "the wasm struct callback produced unexpected output",
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

    let build = Command::new(env!("CARGO_BIN_EXE_kira"))
        .args(["build", "--device", "wasm32", entry.to_str().unwrap()])
        .output()
        .expect("run kira");
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
