//! Library builds must agree across the three backends.
//!
//! A library has no entrypoint, so it cannot be *run* — which means the usual
//! stdout-and-exit-status comparison has nothing to compare. What parity means
//! here instead is that all three backends make the same two decisions about
//! the same package: `build` succeeds and produces an artifact, and `run` is
//! refused with the same reason.
//!
//! That is the whole of what step 0 promises. Which functions a consumer may
//! call, and how, is `@Export`'s to decide.

use super::*;

/// A library package: `package.kira` plus one source file with no `@Main`.
///
/// Returns the source path. Each package gets its own directory, as every
/// parity case does, because `.kira-build` artifacts land beside the source.
fn write_library(source: &str) -> PathBuf {
    let path = write_source(source);
    let directory = path.parent().expect("package directory");
    std::fs::write(
        directory.join("package.kira"),
        "Package parity {\n    let version = \"0.1.0\"\n    let kind = .Library\n}\n",
    )
    .expect("write package.kira");
    path
}

/// Builds `path` on one backend.
fn build_on(source_path: &std::path::Path, backend: &str) -> Output {
    Command::new(env!("CARGO_BIN_EXE_kirac"))
        .args(["build", "--backend", backend, source_path.to_str().unwrap()])
        .output()
        .expect("run kirac")
}

/// A library exercising the value types that cross no boundary yet but must
/// still compile on every engine.
const LIBRARY: &str = "\
function add(a: Int, b: Int) -> Int { return a + b }\n\
function scale(value: Int, by: Int) -> Int { return value * by }\n\
function greeting(name: String) -> String { return \"hello \" + name }\n\
function isPositive(value: Int) -> Bool { return value > 0 }\n\
class Button {\n\
    var title: String = \"\"\n\
    var width: Int = 120\n\
    function label() -> String { return self.title }\n\
}\n\
function makeButton(title: String) -> Button { var b = Button() b.title = title return b }";

#[test]
fn every_backend_builds_a_library() {
    let path = write_library(LIBRARY);
    let runs: Vec<(&str, Output)> = BACKENDS
        .iter()
        .map(|backend| (*backend, build_on(&path, backend)))
        .collect();
    let _ = std::fs::remove_dir_all(path.parent().expect("package directory"));

    for (backend, run) in &runs {
        assert!(
            run.status.success(),
            "the {backend} backend failed to build a library:\nstderr: {}",
            String::from_utf8_lossy(&run.stderr),
        );
        assert!(
            String::from_utf8_lossy(&run.stdout).contains("Successfully built"),
            "the {backend} backend built nothing it would admit to:\nstdout: {}",
            String::from_utf8_lossy(&run.stdout),
        );
    }
}

#[test]
fn every_backend_refuses_to_run_a_library_the_same_way() {
    let path = write_library(LIBRARY);
    let runs: Vec<(&str, Output)> = BACKENDS
        .iter()
        .map(|backend| (*backend, run_on(&path, backend)))
        .collect();
    let _ = std::fs::remove_dir_all(path.parent().expect("package directory"));

    for (backend, run) in &runs {
        assert_eq!(
            run.status.code(),
            Some(1),
            "the {backend} backend did not refuse to run a library:\nstderr: {}",
            String::from_utf8_lossy(&run.stderr),
        );
        assert!(
            String::from_utf8_lossy(&run.stdout).is_empty(),
            "the {backend} backend printed something while refusing to run a \
             library:\nstdout: {}",
            String::from_utf8_lossy(&run.stdout),
        );
        let stderr = String::from_utf8_lossy(&run.stderr);
        // The refusal is above the backend split, so all three say the same
        // thing — which is the point: no backend gets its own opinion about
        // what a library is.
        assert!(
            stderr.contains("cannot run a library"),
            "the {backend} backend refused for a different reason: {stderr}",
        );
    }
}

#[test]
fn a_library_artifact_is_not_an_executable() {
    // The LLVM backend's whole job here is to *not* emit a C `main`. Proving it
    // by absence: no executable named after the package appears beside the
    // source, where a program build puts one.
    let path = write_library(LIBRARY);
    let directory = path.parent().expect("package directory").to_path_buf();
    let build = build_on(&path, "llvm");
    let artifacts = directory.join(".kira-build");
    let executable = artifacts.join("program");
    let is_executable = executable.is_file();
    // What a Rust consumer links: one self-contained static archive under
    // `lib/`, beside where the VM engine writes its `.kbc`.
    let archive = artifacts.join("lib").join("libparity.a");
    let archive_exists = archive.is_file();
    let _ = std::fs::remove_dir_all(&directory);

    assert!(
        build.status.success(),
        "the llvm backend failed to build a library:\nstderr: {}",
        String::from_utf8_lossy(&build.stderr),
    );
    assert!(
        !is_executable,
        "a library build emitted an executable at {}",
        executable.display(),
    );
    assert!(
        archive_exists,
        "a library build emitted no static archive at {}",
        archive.display(),
    );
}

// ---------------------------------------------------------------------------
// The `@Export` surface
//
// The VM engine serves it and the other two do not yet, so parity here is not
// "all three agree" — it is that all three see the *same* export surface, and
// each says what it does with it. The surface is frontend work above the
// backend split, so the names must be identical on all three; the verdicts
// differ, by name, with a reason and a redirection.
// ---------------------------------------------------------------------------

/// The motivating library, in the shapes v1 supports: a handle-eligible class,
/// a constructor-shaped export, and scalars in both directions.
const EXPORTING_LIBRARY: &str = "\
@Export\n\
class Button {\n\
    var title: String = \"\"\n\
    var width: Int = 120\n\
    function label() -> String { return self.title }\n\
}\n\
@Export\n\
function makeButton(title: String) -> Button { var b = Button() b.title = title return b }\n\
@Export\n\
function buttonWidth(b: Button) -> Int { return b.width }\n\
@Export\n\
function clickAt(b: Button, x: Int) -> Bool { return x >= 0 && x < b.width }";

#[test]
fn every_backend_sees_the_same_export_surface() {
    // The one thing that must not differ. Two engines build this surface and
    // one still refuses it, but all three read the identical derived consumer
    // names — a backend that disagreed here would have grown its own opinion
    // about what an export is, which is exactly what putting the checks in the
    // frontend prevents.
    //
    // The wrapper is read *after each build* rather than once at the end,
    // because both engines write their crate to the same path: reading once
    // would only ever check whichever ran last.
    let path = write_library(EXPORTING_LIBRARY);
    let package = path.parent().expect("package directory").to_path_buf();
    let generated = package
        .join(".kira-build")
        .join("rust")
        .join("parity")
        .join("src")
        .join("lib.rs");

    let runs: Vec<(&str, Output, String)> = BACKENDS
        .iter()
        .map(|backend| {
            let run = build_on(&path, backend);
            let wrapper = std::fs::read_to_string(&generated).unwrap_or_default();
            (*backend, run, wrapper)
        })
        .collect();
    let _ = std::fs::remove_dir_all(&package);

    for (backend, run, wrapper) in &runs {
        let stderr = String::from_utf8_lossy(&run.stderr);
        let stdout = String::from_utf8_lossy(&run.stdout);
        match *backend {
            // Both engines build the surface, and build the *same* surface:
            // same three methods, same newtype, from one generator over one
            // frontend. That identity is the whole parity claim — a consumer's
            // code does not change when the engine does.
            "vm" | "llvm" => {
                assert!(
                    run.status.success(),
                    "the {backend} engine failed to build an export:\nstderr: {stderr}",
                );
                assert!(stdout.contains("3 exports"), "stdout: {stdout}");
                for method in ["make_button", "button_width", "click_at"] {
                    assert!(
                        wrapper.contains(&format!("pub fn {method}(")),
                        "the {backend} engine's wrapper has no `{method}`:\n{wrapper}",
                    );
                }
                assert!(
                    wrapper.contains("pub struct Button"),
                    "the {backend} engine's wrapper has no handle type:\n{wrapper}",
                );
            }
            _ => {
                assert_eq!(
                    run.status.code(),
                    Some(1),
                    "the {backend} backend did not refuse to build an export:\nstderr: {stderr}",
                );
                assert!(
                    stdout.contains("Failed to build") && !stdout.contains("Successfully built"),
                    "the {backend} backend claimed a build it refused:\nstdout: {stdout}",
                );
                assert!(
                    stderr.contains("library export is not built yet"),
                    "the {backend} backend refused for a different reason: {stderr}",
                );
                assert!(
                    stderr.contains("make_button, button_width, click_at"),
                    "the {backend} backend named a different surface: {stderr}",
                );
            }
        }
    }
}

#[test]
fn the_two_engines_generate_the_same_public_surface_over_different_internals() {
    // Stated as its own test because it is the feature's central claim, and a
    // claim checked only as a side effect of another test is a claim nobody
    // notices breaking. The two crates share every public item and share none
    // of their internals: one embeds bytecode, the other declares C symbols.
    let path = write_library(EXPORTING_LIBRARY);
    let package = path.parent().expect("package directory").to_path_buf();
    let generated = package
        .join(".kira-build")
        .join("rust")
        .join("parity")
        .join("src")
        .join("lib.rs");

    let vm_built = build_on(&path, "vm");
    let vm_wrapper = std::fs::read_to_string(&generated).unwrap_or_default();
    let native_built = build_on(&path, "llvm");
    let native_wrapper = std::fs::read_to_string(&generated).unwrap_or_default();
    let _ = std::fs::remove_dir_all(&package);

    assert!(vm_built.status.success(), "the vm engine failed");
    assert!(native_built.status.success(), "the native engine failed");

    for item in [
        "pub struct Parity",
        "pub struct Button",
        "pub fn load()",
        "pub fn make_button(",
        "pub fn button_width(",
        "pub fn click_at(",
        "pub type Error = kira_main::Error;",
    ] {
        assert!(vm_wrapper.contains(item), "the vm crate has no `{item}`");
        assert!(
            native_wrapper.contains(item),
            "the native crate has no `{item}`"
        );
    }

    // And the internals really are different, so the agreement above is two
    // engines agreeing rather than one engine generated twice.
    assert!(vm_wrapper.contains("include_bytes!"), "{vm_wrapper}");
    // Comments stripped: the VM crate's prose says it contains no `unsafe`,
    // and matching on that sentence would make the claim check itself.
    let vm_code: String = vm_wrapper
        .lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(!vm_code.contains("unsafe"), "{vm_code}");
    assert!(
        native_wrapper.contains("kira_lib_parity_abi_1"),
        "{native_wrapper}"
    );
    assert!(
        !native_wrapper.contains("include_bytes!"),
        "{native_wrapper}"
    );
}

#[test]
fn checking_an_exporting_library_stops_at_a_clean_frontend() {
    // The refusal the test above proves is the *engine's*, not the frontend's:
    // the same package no backend will build passes `check`, because `check`
    // stops at the frontend and the frontend is where this step is finished.
    //
    // Deliberately one run, not one per backend: `kirac check` takes no
    // `--backend` (pipeline.rs::check reads only a path), so looping the
    // backends here would run the identical command three times and prove
    // nothing about any of them. What makes this frontend-wide is that the
    // export checks sit above the backend split at all.
    let path = write_library(EXPORTING_LIBRARY);
    let run = Command::new(env!("CARGO_BIN_EXE_kirac"))
        .args(["check", path.to_str().unwrap()])
        .output()
        .expect("run kirac");
    let _ = std::fs::remove_dir_all(path.parent().expect("package directory"));

    assert!(
        run.status.success(),
        "checking an exporting library failed:\nstderr: {}",
        String::from_utf8_lossy(&run.stderr),
    );
}
