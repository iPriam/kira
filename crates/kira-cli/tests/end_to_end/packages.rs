//! Library packages.
//!
//! `kind = .Library` in `package.kira` is what makes a package a library, and
//! these prove the three things that follow from it end to end, through the real
//! binary: a library with no `@Main` checks clean, running one is refused by
//! name, and building one produces an artifact on each backend the CI machine
//! has.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use crate::{LIBRARY_SOURCE, check_source, kira, write_package, write_program};

/// A unique package tree that removes all build artifacts with itself.
struct PackageTree(PathBuf);

impl PackageTree {
    fn new(tag: &str) -> Self {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "kira-e2e-packages-{tag}-{}-{unique}",
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
    let output = kira(&["check", path.to_str().unwrap()]);
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
    let output = kira(&["check", path.to_str().unwrap()]);
    let _ = std::fs::remove_dir_all(path.parent().expect("package directory"));
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("KSEM011"), "{stderr}");
}

#[test]
fn a_library_declaring_main_is_refused() {
    let path = write_package(".Library", "@Main function main() { print(1) return }");
    let output = kira(&["check", path.to_str().unwrap()]);
    let _ = std::fs::remove_dir_all(path.parent().expect("package directory"));
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("KSEM255"), "{stderr}");
}

#[test]
fn running_a_library_is_refused_by_name_with_a_reason() {
    let path = write_package(".Library", LIBRARY_SOURCE);
    let output = kira(&["run", path.to_str().unwrap()]);
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
    let output = kira(&["build", "--backend", "vm", path.to_str().unwrap()]);
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
fn a_library_builds_for_the_web() {
    let path = write_package(".Library", LIBRARY_SOURCE);
    let directory = path.parent().expect("package directory").to_path_buf();
    let output = kira(&[
        "build",
        "--backend",
        "llvm",
        "--device",
        "wasm32",
        path.to_str().unwrap(),
    ]);
    let web = directory.join(".kira-build").join("web");
    let wasm = web.join("uifoundation.wasm");
    let javascript = web.join("uifoundation.js");
    let stderr = String::from_utf8_lossy(&output.stderr);
    let artifacts = (wasm.is_file(), javascript.is_file());
    let _ = std::fs::remove_dir_all(directory);
    assert!(output.status.success(), "{stderr}");
    assert_eq!(artifacts, (true, true), "{web:?}");
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
    let output = kira(&["check", path.to_str().unwrap()]);
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

    let output = kira(&["check", editor_arg]);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(output.status.code().is_some(), "process crashed: {stderr}");
    assert!(!stderr.contains("panicked at"), "{stderr}");
    assert!(!stderr.contains("stack backtrace"), "{stderr}");
    // Every package the editor's manifest declares has to resolve. Asserted per
    // dependency rather than by finding one of that package's *diagnostics* in
    // the output: a package that checks clean renders no path, so keying the
    // gate on a rendered path would make it fail the moment the migration fixed
    // that package — which is exactly what happened to the `Core` spelling of
    // this check.
    for dependency in ["Editor", "KiraGraphics", "Core", "Graphics"] {
        assert!(
            !stderr.contains(&format!("Dependency `{dependency}`")),
            "`{dependency}` dependency resolution failed: {stderr}"
        );
        assert!(
            !stderr.contains(&format!(
                "error[KSEM032]: Kira could not find a module for import `{dependency}`"
            )),
            "the `{dependency}` import was unresolved: {stderr}"
        );
    }
    // A dependency whose sources never loaded takes every name it declares with
    // it, so the failure mode this gate exists to catch is a flood of
    // undefined-name diagnostics rather than one message. Any single package
    // going missing puts the count far past what the corpus carries.
    let undefined = stderr
        .lines()
        .filter(|line| line.contains("[KSEM060]") || line.contains("[KSEM061]"))
        .count();
    assert!(
        undefined < 200,
        "{undefined} undefined-name diagnostics: a dependency package's sources \
         did not load. {stderr}"
    );
    for line in stderr.lines().filter(|line| {
        let trimmed = line.trim_start();
        trimmed.starts_with("error[") || trimmed.starts_with("warning[")
    }) {
        // A bare package-name import now aggregates every `.kira` file of the
        // dependency, so files using surface this subset does not lex, parse, or
        // expand yet surface honest `KLEX`/`KPAR`/`KMAC` diagnostics against
        // their own lines rather than going unread. Those are real, rendered
        // compiler codes, which is all this gate asserts.
        assert!(
            line.contains("[KSEM")
                || line.contains("[KPAR")
                || line.contains("[KPK")
                || line.contains("[KLEX")
                || line.contains("[KMAC"),
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

    let checked = kira(&["check", app]);
    assert!(
        checked.status.success(),
        "{}",
        String::from_utf8_lossy(&checked.stderr)
    );

    let mut expected_stdout = None;
    for backend in ["vm", "llvm", "hybrid"] {
        let run = kira(&["run", "--backend", backend, app]);
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

        let build = kira(&["build", "--backend", backend, app]);
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
fn a_runtime_app_with_an_unused_native_library_dependency_runs_hybrid() {
    let tree = PackageTree::new("hybrid-reachability");
    tree.write(
        "app/package.kira",
        r#"Package RuntimeApp {
    let kind = .App
    let dependencies = [
        Dependency { name: "NativeLibrary", path: "../native-library" }
    ]
}
"#,
    );
    tree.write(
        "native-library/package.kira",
        r#"Package NativeLibrary {
    let kind = .Library
    let moduleRoot = "NativeLibrary"
}
"#,
    );
    tree.write(
        "native-library/app/NativeLibrary.kira",
        "@Native function unusedNative() -> Int { return 7 }\n\
         function libraryValue() -> Int { return 42 }",
    );
    tree.write(
        "app/app/main.kira",
        "import NativeLibrary\n@Main function main() { print(libraryValue()) return }",
    );
    let app = tree.path().join("app");
    let app = app.to_str().expect("UTF-8 temp path");

    let checked = kira(&["check", app]);
    assert!(
        checked.status.success(),
        "{}",
        String::from_utf8_lossy(&checked.stderr)
    );

    let run = kira(&["run", "--backend", "hybrid", app]);
    assert!(
        run.status.success(),
        "hybrid run failed: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout), "42\n");
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
    // At the package root, not in `app/`: the artifacts belong to the package
    // rather than to the directory its entrypoint happens to sit in.
    let artifacts = app.join(".kira-build");

    let default_run = kira(&["run", "--emit-llvm-ir", app_arg]);
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
    let vm_run = kira(&["run", "--backend", "vm", "--emit-llvm-ir", app_arg]);
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
    let web_artifact = app.join(".kira-build/web/main.js");

    let output = kira(&["build", "--backend", "vm", app_arg]);
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

/// A successful VM build writes the bytecode module it reports.
#[test]
fn a_program_build_writes_the_bytecode_it_reports() {
    let path = write_program(
        "import Foundation\n@Main function main() { printLine(\"built\") return }\n",
        &[],
    );
    let directory = path.parent().expect("program directory").to_path_buf();
    let output = kira(&["build", "--backend", "vm", path.to_str().unwrap()]);
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let bytecode = directory.join(".kira-build").join("main.kbc");
    let written = std::fs::read(&bytecode);
    let _ = std::fs::remove_dir_all(&directory);

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    // The path is named rather than left for the reader to guess, the way every
    // other backend's success line names one.
    assert!(
        stdout.contains("Successfully built") && stdout.contains("main.kbc"),
        "{stdout:?}"
    );
    let written = written.expect("the reported bytecode is on disk");
    // A real module, not an empty file standing in for one.
    assert!(
        written.starts_with(&kira_bytecode::module::MAGIC),
        "the artifact is not a bytecode module: {:?}",
        &written[..written.len().min(8)]
    );
}

/// Two packages may each declare the same nominal names — struct, enum,
/// distinct, class, generic enum, trait, construct family — and the program that imports both keeps
/// them apart everywhere identity is asked: each package's functions take
/// their own types, generic instantiations over them are separate rows, and
/// erased values of the two never compare equal.
#[test]
fn same_named_declarations_in_two_packages_are_different_types_on_every_host_backend() {
    let tree = PackageTree::new("nominal-identity");
    tree.write(
        "app/package.kira",
        r#"Package IdentityApp {
    let kind = .App
    let dependencies = [
        Dependency { name: "Alpha", path: "../alpha" },
        Dependency { name: "Beta", path: "../beta" }
    ]
}
"#,
    );
    for (package, weight) in [("Alpha", 1), ("Beta", 100)] {
        let lower = package.to_lowercase();
        tree.write(
            &format!("{lower}/package.kira"),
            &format!(
                "Package {package} {{\n    let kind = .Library\n    let moduleRoot = \"{package}\"\n}}\n"
            ),
        );
        tree.write(
            &format!("{lower}/app/{package}.kira"),
            &format!(
                r#"struct Point {{
    let x: Int
}}

enum Shade {{
    Light
    Dark
}}

distinct Id = Int

class Widget {{
    var size: Int = {weight}
}}

enum Box<T> {{
    Full(T)
    Empty
}}

function {lower}Point() -> Point {{
    return Point(x: {weight})
}}

function {lower}Weigh(p: borrow Point) -> Int {{
    return p.x
}}

function {lower}Shade() -> Shade {{
    return .Dark
}}

function {lower}ShadeCode(s: Shade) -> Int {{
    match s {{
        Light -> return {weight}
        Dark -> return {weight} * 2
    }}
}}

function {lower}Id() -> Id {{
    return Id({weight})
}}

function {lower}Raw(id: Id) -> Int {{
    return id.raw
}}

function {lower}Widget() -> Widget {{
    return Widget()
}}

function {lower}Size(w: borrow Widget) -> Int {{
    return w.size
}}

function {lower}Boxed() -> Box<Point> {{
    return .Full({lower}Point())
}}

function {lower}Unbox(b: Box<Point>) -> Int {{
    match b {{
        Full(p) -> return {lower}Weigh(p)
        Empty -> return 0
    }}
}}

function {lower}Erased() -> Any {{
    return {lower}Point()
}}

trait Named {{
    function label(borrow self) -> Int
}}

extend Point: Named {{
    function label(borrow self) -> Int {{
        return x * 3
    }}
}}

function {lower}Labelled() -> Named {{
    return {lower}Point()
}}

function {lower}Label(value: Named) -> Int {{
    return value.label()
}}

construct Shape {{
    @Required function area() -> Int
}}

construct Square(side: Int) extends Shape {{
    function area() -> Int {{
        return side * side
    }}
}}

function {lower}Shape() -> Any Shape {{
    return Square(side: {weight})
}}

function {lower}Area(shape: Any Shape) -> Int {{
    return shape.area()
}}
"#
            ),
        );
    }
    tree.write(
        "app/app/main.kira",
        r#"import Alpha
import Beta

@Main
function main() {
    print(alphaWeigh(alphaPoint()) + betaWeigh(betaPoint()))
    print(alphaShadeCode(alphaShade()) + betaShadeCode(betaShade()))
    print(alphaRaw(alphaId()) + betaRaw(betaId()))
    print(alphaSize(alphaWidget()) + betaSize(betaWidget()))
    print(alphaUnbox(alphaBoxed()) + betaUnbox(betaBoxed()))
    print(alphaErased() == betaErased())
    print(alphaErased() == alphaErased())
    print(alphaLabel(alphaLabelled()) + betaLabel(betaLabelled()))
    print(alphaArea(alphaShape()) + betaArea(betaShape()))
    return
}
"#,
    );
    let app = tree.path().join("app");
    let app = app.to_str().expect("UTF-8 temp path");

    let checked = kira(&["check", app]);
    assert!(
        checked.status.success(),
        "{}",
        String::from_utf8_lossy(&checked.stderr)
    );
    for backend in ["vm", "llvm", "hybrid"] {
        let run = kira(&["run", "--backend", backend, app]);
        assert!(
            run.status.success(),
            "{backend} run failed: {}",
            String::from_utf8_lossy(&run.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&run.stdout),
            "101\n202\n101\n101\n101\nfalse\ntrue\n303\n10001\n",
            "unexpected {backend} output"
        );
    }
}
