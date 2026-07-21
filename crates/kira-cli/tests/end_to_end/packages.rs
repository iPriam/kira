//! Library packages.
//!
//! `kind = .Library` in `package.kira` is what makes a package a library, and
//! these prove the three things that follow from it end to end, through the real
//! binary: a library with no `@Main` checks clean, running one is refused by
//! name, and building one produces an artifact on each backend the CI machine
//! has.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use crate::{LIBRARY_SOURCE, check_source, kirac, write_package};

/// A unique package tree that removes all build artifacts with itself.
struct PackageTree(PathBuf);

impl PackageTree {
    fn new(tag: &str) -> Self {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "kirac-e2e-packages-{tag}-{}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).expect("create package tree");
        Self(root)
    }

    fn path(&self) -> &Path {
        &self.0
    }

    fn write(&self, relative: &str, text: &str) -> PathBuf {
        let path = self.0.join(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create package fixture directory");
        }
        std::fs::write(&path, text).expect("write package fixture");
        path
    }
}

impl Drop for PackageTree {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn a_library_without_main_checks_clean() {
    let path = write_package(".Library", LIBRARY_SOURCE);
    let output = kirac(&["check", path.to_str().unwrap()]);
    let _ = std::fs::remove_dir_all(path.parent().expect("package directory"));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stderr}");
    assert!(
        !stderr.contains("KSEM011"),
        "a library needs no `@Main`: {stderr}"
    );
}

#[test]
fn the_same_source_in_an_app_package_is_still_ksem011() {
    // The exemption comes from the manifest and nowhere else. Same bytes, same
    // command, different `kind` — and the entrypoint requirement comes back.
    let path = write_package(".App", LIBRARY_SOURCE);
    let output = kirac(&["check", path.to_str().unwrap()]);
    let _ = std::fs::remove_dir_all(path.parent().expect("package directory"));
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("KSEM011"), "{stderr}");
}

#[test]
fn a_library_declaring_main_is_refused() {
    let path = write_package(".Library", "@Main function main() { print(1) return }");
    let output = kirac(&["check", path.to_str().unwrap()]);
    let _ = std::fs::remove_dir_all(path.parent().expect("package directory"));
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("KSEM158"), "{stderr}");
}

#[test]
fn running_a_library_is_refused_by_name_with_a_reason() {
    let path = write_package(".Library", LIBRARY_SOURCE);
    let output = kirac(&["run", path.to_str().unwrap()]);
    let _ = std::fs::remove_dir_all(path.parent().expect("package directory"));
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("cannot run a library"), "{stderr}");
    // The reason, not just the refusal: a user who is told "no" and not "why"
    // has to guess.
    assert!(stderr.contains("no `@Main` entrypoint"), "{stderr}");
}

#[test]
fn a_library_builds_on_the_vm_backend() {
    // The VM backend is the one CI has, so this is the artifact proof that runs
    // everywhere. It compiles to a real KBC1 module with no entrypoint.
    let path = write_package(".Library", LIBRARY_SOURCE);
    let output = kirac(&["build", "--backend", "vm", path.to_str().unwrap()]);
    let _ = std::fs::remove_dir_all(path.parent().expect("package directory"));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stderr}");
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("Successfully built"),
        "{:?}",
        output.stdout
    );
}

#[test]
fn a_library_cannot_be_built_for_the_web_and_says_why() {
    // The recorded wasm refusal: a library artifact for a JS host needs a
    // string/allocator contract across the module boundary that is undesigned.
    let path = write_package(".Library", LIBRARY_SOURCE);
    let output = kirac(&[
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
        stderr.contains("a library cannot be built as a wasm module yet"),
        "{stderr}"
    );
}

#[test]
fn a_package_with_no_manifest_is_still_an_application() {
    // The default has to hold: a bare `.kira` file is a program, so a missing
    // `@Main` is still an error with no manifest anywhere above it.
    let output = check_source("function add(a: Int, b: Int) -> Int { return a + b }");
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("KSEM011"), "{stderr}");
}

#[test]
fn a_malformed_package_manifest_is_reported_not_ignored() {
    let path = write_package(".Plugin", LIBRARY_SOURCE);
    let output = kirac(&["check", path.to_str().unwrap()]);
    let _ = std::fs::remove_dir_all(path.parent().expect("package directory"));
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("is not a package kind"), "{stderr}");
}

#[test]
fn the_real_editor_directory_resolves_dependency_package_sources() {
    const EDITOR_OVERRIDE: &str = "KIRA_REAL_EDITOR_DIR";
    let editor = std::env::var_os(EDITOR_OVERRIDE)
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../..")
                .join("project-matter/apps/editor")
        });
    if !editor.is_dir() {
        eprintln!(
            "skipping real editor package test: {} is not present; set {EDITOR_OVERRIDE} to override",
            editor.display()
        );
        return;
    }
    let editor_arg = editor.to_str().expect("UTF-8 real editor path");

    let output = kirac(&["check", editor_arg]);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(output.status.code().is_some(), "process crashed: {stderr}");
    assert!(!stderr.contains("panicked at"), "{stderr}");
    assert!(!stderr.contains("stack backtrace"), "{stderr}");
    assert!(
        stderr.contains("/project-matter/modules/core/")
            || stderr.contains("/project-matter/modules/core\\"),
        "Core package sources were not rendered: {stderr}"
    );
    assert!(
        !stderr.contains("Dependency `KiraGraphics`"),
        "KiraGraphics dependency resolution failed: {stderr}"
    );
    assert!(
        !stderr.contains("error[KSEM032]: Kira could not find a module for import `KiraGraphics`"),
        "KiraGraphics import was unresolved: {stderr}"
    );
    for line in stderr.lines().filter(|line| {
        let trimmed = line.trim_start();
        trimmed.starts_with("error[") || trimmed.starts_with("warning[")
    }) {
        // A bare package-name import now aggregates every `.kira` file of the
        // dependency, so files using surface this subset does not lex or parse
        // yet (macro interpolation, `#{...}`) surface honest `KLEX`/`KPAR`
        // diagnostics against their own lines rather than going unread. Those
        // are real, rendered compiler codes, which is all this gate asserts.
        assert!(
            line.contains("[KSEM")
                || line.contains("[KPAR")
                || line.contains("[KPK")
                || line.contains("[KLEX"),
            "diagnostic lacks an honest compiler code: {line}"
        );
        assert!(line.contains("]:"), "diagnostic is not rendered: {line}");
    }
}

#[test]
fn a_multi_package_directory_checks_runs_and_builds_on_every_host_backend() {
    let tree = PackageTree::new("parity");
    tree.write(
        "app/package.kira",
        r#"Package DemoApp {
    let kind = .App
    let dependencies = [
        Dependency { name: "Alpha", path: "../alpha" },
        Dependency { name: "Beta", path: "../beta" }
    ]
}
"#,
    );
    tree.write(
        "alpha/package.kira",
        r#"Package Alpha {
    let kind = .Library
    let moduleRoot = "Alpha"
}
"#,
    );
    tree.write(
        "alpha/app/Alpha.kira",
        "function alphaValue() -> Int { return 19 }",
    );
    tree.write(
        "beta/package.kira",
        r#"Package Beta {
    let kind = .Library
    let moduleRoot = "Beta"
}
"#,
    );
    tree.write(
        "beta/app/Beta.kira",
        "function betaValue() -> Int { return 23 }",
    );
    tree.write(
        "app/app/main.kira",
        "import Alpha\nimport Beta\n@Main function main() { print(alphaValue() + betaValue()) return }",
    );
    let app = tree.path().join("app");
    let app = app.to_str().expect("UTF-8 temp path");

    let checked = kirac(&["check", app]);
    assert!(
        checked.status.success(),
        "{}",
        String::from_utf8_lossy(&checked.stderr)
    );

    let mut expected_stdout = None;
    for backend in ["vm", "llvm", "hybrid"] {
        let run = kirac(&["run", "--backend", backend, app]);
        assert!(
            run.status.success(),
            "{backend} run failed: {}",
            String::from_utf8_lossy(&run.stderr)
        );
        let stdout = String::from_utf8_lossy(&run.stdout).into_owned();
        assert_eq!(stdout, "42\n", "unexpected {backend} output");
        if let Some(expected) = &expected_stdout {
            assert_eq!(&stdout, expected, "{backend} diverged from vm");
        } else {
            expected_stdout = Some(stdout);
        }

        let build = kirac(&["build", "--backend", backend, app]);
        assert!(
            build.status.success(),
            "{backend} build failed: {}",
            String::from_utf8_lossy(&build.stderr)
        );
        assert!(
            String::from_utf8_lossy(&build.stdout).contains("Successfully built"),
            "{backend} build did not report its artifact: {}",
            String::from_utf8_lossy(&build.stdout)
        );
    }
}

#[test]
fn manifest_llvm_default_runs_natively_but_an_explicit_vm_still_wins() {
    let tree = PackageTree::new("defaults");
    tree.write(
        "app/package.kira",
        r#"Package DefaultNative {
    let kind = .App
    let defaults = Defaults { executionMode: Backend.Llvm, buildTarget: BuildTarget.Host }
}
"#,
    );
    tree.write(
        "app/app/main.kira",
        "@Main function main() { print(42) return }",
    );
    let app = tree.path().join("app");
    let app_arg = app.to_str().expect("UTF-8 temp path");
    let artifacts = app.join("app/.kira-build");

    let default_run = kirac(&["run", "--emit-llvm-ir", app_arg]);
    assert!(
        default_run.status.success(),
        "{}",
        String::from_utf8_lossy(&default_run.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&default_run.stdout), "42\n");
    assert!(
        artifacts.join("main.ll").is_file(),
        "manifest LLVM default did not emit native LLVM IR"
    );

    std::fs::remove_dir_all(&artifacts).expect("remove default native artifacts");
    let vm_run = kirac(&["run", "--backend", "vm", "--emit-llvm-ir", app_arg]);
    assert!(
        vm_run.status.success(),
        "{}",
        String::from_utf8_lossy(&vm_run.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&vm_run.stdout), "42\n");
    assert!(
        !artifacts.exists(),
        "explicit VM was overridden by the manifest LLVM default"
    );
}

#[test]
fn an_explicit_vm_backend_outranks_a_manifest_wasm32_target() {
    let tree = PackageTree::new("wasm-default");
    tree.write(
        "app/package.kira",
        r#"Package DefaultWeb {
    let kind = .App
    let defaults = Defaults { executionMode: Backend.Llvm, buildTarget: BuildTarget.Wasm32 }
}
"#,
    );
    tree.write(
        "app/app/main.kira",
        "@Main function main() { print(42) return }",
    );
    let app = tree.path().join("app");
    let app_arg = app.to_str().expect("UTF-8 temp path");
    let web_artifact = app.join("app/.kira-build/web/main.js");

    let output = kirac(&["build", "--backend", "vm", app_arg]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stderr}");
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("Successfully built"),
        "VM build did not report success: {:?}",
        output.stdout
    );
    assert!(
        !stderr.contains("`--device wasm32` overrides `--backend vm`"),
        "a manifest default was reported as an explicit device: {stderr}"
    );
    assert!(
        !web_artifact.exists(),
        "manifest wasm32 target overrode the explicit VM backend"
    );
}
