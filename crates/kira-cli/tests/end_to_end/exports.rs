//! The `@Export` surface.
//!
//! The VM engine serves it: `kira build` in a library package writes the
//! artifact *and* the Rust crate that embeds and calls it, which is the product
//! a consumer depends on. The other two engines are refused by name, each saying
//! what it still owes. All of this runs through the real binary, on a machine
//! with no LLVM — the crate it generates is compiled and called for real by
//! `kira-export-consumer`.

use crate::{kira, write_package};

/// A library that exports the shapes v1 supports: a handle-eligible class, a
/// constructor-shaped export, and scalars both ways.
const EXPORTING_LIBRARY: &str = "@Export\n\
     class Button {\n\
         var title: String = \"\"\n\
         var width: Int = 120\n\
         function label() -> String { return self.title }\n\
     }\n\
     @Export\n\
     function makeButton(title: String) -> Button { \
         var b = Button() b.title = title return b }\n\
     @Export\n\
     function buttonWidth(b: Button) -> Int { return b.width }\n\
     @Export\n\
     function clickAt(b: Button, x: Int) -> Bool { return x >= 0 && x < b.width }";

#[test]
fn an_exporting_library_checks_clean() {
    let path = write_package(".Library", EXPORTING_LIBRARY);
    let output = kira(&["check", path.to_str().unwrap()]);
    let _ = std::fs::remove_dir_all(path.parent().expect("package directory"));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stderr}");
}

#[test]
fn an_export_in_an_app_package_is_refused_by_name() {
    // Same source, `.App` instead of `.Library`, and the marker stops being
    // meaningful. The manifest is what decides, exactly as it does for `@Main`.
    let path = write_package(
        ".App",
        "@Main function main() { print(1) return }\n\
         @Export\nfunction add(a: Int) -> Int { return a }",
    );
    let output = kira(&["check", path.to_str().unwrap()]);
    let _ = std::fs::remove_dir_all(path.parent().expect("package directory"));
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("KSEM256"), "{stderr}");
}

#[test]
fn the_vm_engine_builds_an_export_into_an_artifact_and_a_rust_crate() {
    // The product, through the real binary: a `.kbc` a consumer never sees, and
    // a crate they depend on by path. Both are named on stdout, because a build
    // whose output nobody can find is a build nobody can use.
    let path = write_package(".Library", EXPORTING_LIBRARY);
    let directory = path.parent().expect("package directory").to_path_buf();
    let output = kira(&["build", "--backend", "vm", path.to_str().unwrap()]);
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();

    let build = directory.join(".kira-build");
    let artifact = build.join("lib").join("uifoundation.kbc");
    let generated = build.join("rust").join("uifoundation");
    let present: Vec<bool> = ["Cargo.toml", "README.md", "src/lib.rs", "uifoundation.kbc"]
        .iter()
        .map(|file| generated.join(file).is_file())
        .collect();
    // Read before the directory goes, like `present` above: an `is_file` after
    // the removal is a check of nothing.
    let artifact_present = artifact.is_file();
    let wrapper = std::fs::read_to_string(generated.join("src").join("lib.rs")).unwrap_or_default();
    let _ = std::fs::remove_dir_all(&directory);

    assert!(output.status.success(), "{stderr}");
    assert!(stdout.contains("Successfully built"), "{stdout}");
    assert!(stdout.contains("uifoundation.kbc"), "{stdout}");
    assert!(stdout.contains("3 exports"), "{stdout}");
    assert!(artifact_present, "no artifact at {artifact:?}");
    assert_eq!(present, [true, true, true, true], "{generated:?}");
    // One safe method per export, with the consumer-facing names the frontend
    // derived — the same list the other engines' refusal prints.
    for method in ["make_button", "button_width", "click_at"] {
        assert!(
            wrapper.contains(&format!("pub fn {method}(")),
            "the wrapper has no `{method}`: {wrapper}"
        );
    }
    // And one newtype for the exported class, so a handle is more than a word —
    // generic over the host, so the embedder still chooses where `print` goes.
    assert!(
        wrapper.contains("pub struct Button<H: HostCapabilities = StdoutHost> {"),
        "{wrapper}"
    );
    assert!(wrapper.contains("pub fn load_with(host: H)"), "{wrapper}");
}

#[test]
fn the_hybrid_engine_no_longer_refuses_an_export_on_export_grounds() {
    // The hybrid engine builds this surface now, so nothing is left refusing on
    // *host* engine grounds — the only export refusal that remains is the wasm
    // library artifact, which is about the artifact rather than an engine.
    //
    // Same two legitimate outcomes as the native engine's test above, and for
    // the same reason: the hybrid engine's native half needs LLVM, so a `kira`
    // built without the feature — which is the CI configuration — refuses for
    // the missing backend rather than for exports. What would be a regression is
    // the export refusal coming back, and that is what this pins.
    let path = write_package(".Library", EXPORTING_LIBRARY);
    let output = kira(&["build", "--backend", "hybrid", path.to_str().unwrap()]);
    let _ = std::fs::remove_dir_all(path.parent().expect("package directory"));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("library export is not built yet"),
        "the hybrid engine still refuses an export: {stderr}"
    );
    if output.status.code() != Some(0) {
        assert!(
            stderr.contains("built without the LLVM backend"),
            "the hybrid engine failed for an unexpected reason: {stderr}"
        );
    }
}

#[test]
fn the_native_engine_no_longer_refuses_an_export_on_export_grounds() {
    // The native engine builds this surface now, so whatever it says, it must
    // not be "library export is not built yet" — and with the LLVM backend a
    // hard part of every kira, the build itself must succeed.
    let path = write_package(".Library", EXPORTING_LIBRARY);
    let output = kira(&["build", "--backend", "llvm", path.to_str().unwrap()]);
    let _ = std::fs::remove_dir_all(path.parent().expect("package directory"));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("library export is not built yet"),
        "the native engine still refuses an export: {stderr}"
    );
    assert_eq!(
        output.status.code(),
        Some(0),
        "the native engine must build an exporting library: {stderr}"
    );
}

#[test]
fn switching_engines_in_one_package_leaves_nothing_of_the_other_behind() {
    // Both engines write `.kira-build/rust/<name>/`, because a consumer's `path`
    // dependency names that directory and must not move when the library is
    // rebuilt. The hazard is each engine's own extra file: cargo runs a build
    // script it *finds*, so the native engine's `build.rs` surviving a VM build
    // would keep linking a stale archive into the consumer's binary — silently,
    // in the exact two-build flow the toolchain documents.
    //
    // The native build runs for real — the LLVM backend is part of every
    // kira — and its failure would fail this test rather than being papered
    // over with a planted file.
    let path = write_package(".Library", EXPORTING_LIBRARY);
    let directory = path.parent().expect("package directory").to_path_buf();
    let generated = directory
        .join(".kira-build")
        .join("rust")
        .join("uifoundation");
    let script = generated.join("build.rs");
    let bytecode = generated.join("uifoundation.kbc");

    let vm = kira(&["build", "--backend", "vm", path.to_str().unwrap()]);
    let first = (vm.status.success(), bytecode.is_file(), script.is_file());

    let native = kira(&["build", "--backend", "llvm", path.to_str().unwrap()]);
    assert!(
        native.status.success(),
        "the native build must succeed: {}",
        String::from_utf8_lossy(&native.stderr)
    );
    // The native engine took over the directory: its script is there and the
    // VM engine's embedded bytecode is gone.
    assert!(script.is_file(), "the native build wrote no build script");
    assert!(
        !bytecode.is_file(),
        "the VM engine's bytecode survived a native build"
    );
    // And the archive it points at is named absolutely. Cargo reads this
    // script from the generated crate's directory, wherever a consumer put
    // it, so a relative path resolves somewhere else entirely and fails the
    // link on an archive sitting exactly where it was left.
    let text = std::fs::read_to_string(&script).expect("read build.rs");
    let search = text
        .lines()
        .find_map(|line| line.split_once("cargo:rustc-link-search=native="))
        .map(|(_, rest)| rest.trim_end_matches("\");").to_owned())
        .unwrap_or_default();
    assert!(
        search.starts_with('/'),
        "the generated build script's link search path is relative: {search}"
    );

    let vm_again = kira(&["build", "--backend", "vm", path.to_str().unwrap()]);
    let back = (
        vm_again.status.success(),
        bytecode.is_file(),
        script.is_file(),
    );
    let manifest = std::fs::read_to_string(generated.join("Cargo.toml")).unwrap_or_default();
    let _ = std::fs::remove_dir_all(&directory);

    assert_eq!(
        first,
        (true, true, false),
        "the first VM build: {}",
        String::from_utf8_lossy(&vm.stderr)
    );
    assert_eq!(
        back,
        (true, true, false),
        "the build script survived the switch back to the VM: {}",
        String::from_utf8_lossy(&vm_again.stderr)
    );
    assert!(manifest.contains("\nbuild = false\n"), "{manifest}");
}

#[test]
fn the_web_refuses_to_build_an_export_too() {
    let path = write_package(".Library", EXPORTING_LIBRARY);
    let output = kira(&[
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
        stderr.contains("library export is not built yet"),
        "{stderr}"
    );
    assert!(stderr.contains("`--device wasm32`"), "{stderr}");
    assert!(
        stderr.contains("string/allocator contract"),
        "the wasm reason was missing: {stderr}"
    );
}
