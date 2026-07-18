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
// Step 1 lands the frontend only, so parity here is a refusal: all three
// backends see the same checked export surface and all three decline to build
// it. What makes that a parity result rather than three separate "no"s is that
// they agree on the verdict, the exit status, and the surface they name — each
// differing only in which engine's missing piece it reports, which is the one
// thing that genuinely differs between them.
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
fn every_backend_refuses_to_build_an_export_the_same_way() {
    let path = write_library(EXPORTING_LIBRARY);
    let runs: Vec<(&str, Output)> = BACKENDS
        .iter()
        .map(|backend| (*backend, build_on(&path, backend)))
        .collect();
    let _ = std::fs::remove_dir_all(path.parent().expect("package directory"));

    for (backend, run) in &runs {
        assert_eq!(
            run.status.code(),
            Some(1),
            "the {backend} backend did not refuse to build an export:\nstderr: {}",
            String::from_utf8_lossy(&run.stderr),
        );
        let stdout = String::from_utf8_lossy(&run.stdout);
        assert!(
            stdout.contains("Failed to build") && !stdout.contains("Successfully built"),
            "the {backend} backend claimed a build it refused:\nstdout: {stdout}",
        );
        let stderr = String::from_utf8_lossy(&run.stderr);
        assert!(
            stderr.contains("library export is not built yet"),
            "the {backend} backend refused for a different reason: {stderr}",
        );
        // The surface itself is frontend work, above the backend split, so all
        // three must report the identical derived names. A backend that
        // disagreed here would have grown its own opinion about what an export
        // is, which is exactly what putting the checks in the frontend prevents.
        assert!(
            stderr.contains("make_button, button_width, click_at"),
            "the {backend} backend named a different surface: {stderr}",
        );
    }
}

#[test]
fn every_backend_agrees_an_exporting_library_checks_clean() {
    // The refusal is the *engine's*, not the frontend's: the same package that
    // no backend will build passes `check` on all three, because `check` stops
    // at the frontend and the frontend is where this step is finished.
    let path = write_library(EXPORTING_LIBRARY);
    let runs: Vec<(&str, Output)> = BACKENDS
        .iter()
        .map(|backend| {
            let run = Command::new(env!("CARGO_BIN_EXE_kirac"))
                .args(["check", path.to_str().unwrap()])
                .output()
                .expect("run kirac");
            (*backend, run)
        })
        .collect();
    let _ = std::fs::remove_dir_all(path.parent().expect("package directory"));

    for (backend, run) in &runs {
        assert!(
            run.status.success(),
            "checking an exporting library failed for {backend}:\nstderr: {}",
            String::from_utf8_lossy(&run.stderr),
        );
    }
}
