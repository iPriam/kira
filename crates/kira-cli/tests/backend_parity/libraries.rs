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
    let library_exists = std::fs::read_dir(&artifacts)
        .map(|entries| {
            entries.filter_map(Result::ok).any(|entry| {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                name.ends_with(".dylib") || name.ends_with(".so") || name.ends_with(".dll")
            })
        })
        .unwrap_or(false);
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
        library_exists,
        "a library build emitted no shared library into {}",
        artifacts.display(),
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
    // The one thing that must not differ. The VM builds it and the other two
    // refuse it, but all three read the identical derived consumer names — a
    // backend that disagreed here would have grown its own opinion about what
    // an export is, which is exactly what putting the checks in the frontend
    // prevents.
    let path = write_library(EXPORTING_LIBRARY);
    let runs: Vec<(&str, Output)> = BACKENDS
        .iter()
        .map(|backend| (*backend, build_on(&path, backend)))
        .collect();
    let generated = path
        .parent()
        .expect("package directory")
        .join(".kira-build")
        .join("rust")
        .join("parity")
        .join("src")
        .join("lib.rs");
    let wrapper = std::fs::read_to_string(&generated).unwrap_or_default();
    let _ = std::fs::remove_dir_all(path.parent().expect("package directory"));

    for (backend, run) in &runs {
        let stderr = String::from_utf8_lossy(&run.stderr);
        let stdout = String::from_utf8_lossy(&run.stdout);
        match *backend {
            "vm" => {
                assert!(
                    run.status.success(),
                    "the vm engine failed to build an export:\nstderr: {stderr}",
                );
                assert!(stdout.contains("3 exports"), "stdout: {stdout}");
                for method in ["make_button", "button_width", "click_at"] {
                    assert!(
                        wrapper.contains(&format!("pub fn {method}(")),
                        "the generated wrapper has no `{method}`:\n{wrapper}",
                    );
                }
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
