//! End-to-end tests driving the built `kirac` binary over real `.kira` files.
//!
//! These exercise the whole pipeline — lexer, parser, salsa analysis, IR,
//! bytecode, VM — plus diagnostic rendering and process exit codes, the way a
//! user invokes it.

use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};

/// Writes `source` to a uniquely-named temp `.kira` file and returns its path.
fn write_source(source: &str) -> PathBuf {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    let path = std::env::temp_dir().join(format!("kirac_e2e_{pid}_{unique}.kira"));
    std::fs::write(&path, source).expect("write temp source");
    path
}

fn kirac(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_kirac"))
        .args(args)
        .output()
        .expect("run kirac")
}

fn run_source(source: &str) -> std::process::Output {
    let path = write_source(source);
    let output = kirac(&["run", path.to_str().unwrap()]);
    let _ = std::fs::remove_file(&path);
    output
}

fn check_source(source: &str) -> std::process::Output {
    let path = write_source(source);
    let output = kirac(&["check", path.to_str().unwrap()]);
    let _ = std::fs::remove_file(&path);
    output
}

#[test]
fn runs_a_program_and_prints_its_output() {
    let output = run_source(
        "@Main function main() { print(runtimeCount()) print(doubleValue(21)) return }\n\
         function runtimeCount() -> Int { return 3 }\n\
         function doubleValue(value: Int) -> Int { return value + value }",
    );
    assert!(output.status.success());
    assert_eq!(String::from_utf8_lossy(&output.stdout), "3\n42\n");
}

#[test]
fn recursion_and_control_flow_execute() {
    let output = run_source(
        "@Main function main() { var i = 0 var s = 0 while i < 5 { s = s + i i = i + 1 } print(s) print(fib(10)) return }\n\
         function fib(n: Int) -> Int { if n < 2 { return n } return fib(n - 1) + fib(n - 2) }",
    );
    assert!(output.status.success());
    assert_eq!(String::from_utf8_lossy(&output.stdout), "10\n55\n");
}

#[test]
fn string_operations_execute() {
    let output = run_source(
        "@Main function main() { let a = \"foo\" let b = \"bar\" print(a + b) print(a == b) return }",
    );
    assert!(output.status.success());
    assert_eq!(String::from_utf8_lossy(&output.stdout), "foobar\nfalse\n");
}

#[test]
fn check_accepts_a_clean_program() {
    let output = check_source("@Main function main() { print(1) return }");
    assert!(output.status.success());
}

#[test]
fn undefined_name_fails_with_a_rendered_diagnostic() {
    let output = run_source("@Main function main() { print(missing) return }");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("KSEM060"), "stderr was: {stderr}");
    assert!(
        stderr.contains('^'),
        "diagnostic should show a caret: {stderr}"
    );
    // A rejected program produces no stdout.
    assert!(output.stdout.is_empty());
}

#[test]
fn missing_main_is_rejected() {
    let output = run_source("function f() -> Int { return 1 }");
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("KSEM011"));
}

#[test]
fn unsupported_construct_is_rejected_cleanly() {
    // A `construct` is outside the subset: it must not crash the compiler, and
    // it must be reported as not-yet-supported rather than silently ignored.
    let output = run_source("construct Mode { }\n@Main function main() { print(1) return }");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("KSEM900"), "stderr was: {stderr}");
    assert!(stderr.contains("not supported yet"));
}

#[test]
fn a_class_declaration_compiles_and_runs() {
    // The counterpart to the case above: a `class` used to be reported as
    // unsupported, and is now ordinary language surface. An inherited method
    // reads a field default the subclass overrode, and a parent-qualified call
    // runs the parent body against this instance.
    let output = run_source(
        "class Account { var balance: Int = 100\n let rate: Int = 2\n \
           function gross() -> Int { return self.balance * self.rate } }\n\
         class Savings extends Account { override let rate = 5\n \
           function bonus() -> Int { return Account.gross() + self.balance } }\n\
         @Main function main() { print(Savings().gross()) print(Savings().bonus()) return }",
    );
    assert!(
        output.status.success(),
        "stderr was: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout), "500\n600\n");
}

#[test]
fn an_enum_declaration_compiles_and_runs() {
    // The counterpart to the case above: an enum used to be reported as
    // unsupported, and is now ordinary language surface. A leading-dot member
    // resolves against the expected type, and `==` compares discriminants.
    let output = run_source(
        "enum Color { Red Green Blue }\n\
         function rank(c: Color) -> Int { if c == .Green { return 2 } return 1 }\n\
         @Main function main() { print(rank(.Green)) return }",
    );
    assert!(
        output.status.success(),
        "stderr was: {}",
        String::from_utf8_lossy(&output.stderr),
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout), "2\n");
}

#[test]
fn a_struct_declaration_compiles_and_runs() {
    // The counterpart to the case above: a struct used to be reported as
    // unsupported, and is now ordinary language surface.
    let output = run_source(
        "struct Point { var x: Int  var y: Int = 4 }\n\
         @Main function main() { let p = Point { x = 3 } print(p.x + p.y) return }",
    );
    assert!(
        output.status.success(),
        "stderr was: {}",
        String::from_utf8_lossy(&output.stderr),
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout), "7\n");
}

#[test]
fn a_struct_method_compiles_and_runs() {
    let output = run_source(
        "struct P { var x: Int\n function doubled() -> Int { return x * 2 } }\n\
         @Main function main() { let p = P { x = 21 } print(p.doubled()) return }",
    );
    assert!(
        output.status.success(),
        "stderr was: {}",
        String::from_utf8_lossy(&output.stderr),
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout), "42\n");
}

#[test]
fn type_error_is_rejected() {
    let output = run_source("@Main function main() { print(1 + true) return }");
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("KSEM071"));
}

#[test]
fn missing_return_path_is_rejected_before_execution() {
    // The C1 soundness hole: without the definite-return check this printed an
    // empty line and exited 0; it must now be a compile error.
    let output = run_source(
        "function f(n: Int) -> Int { if n > 100 { return 1 } }\n\
         @Main function main() { print(f(5)) return }",
    );
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("KSEM033"));
    assert!(output.stdout.is_empty());
}

#[test]
fn newline_continuation_matches_the_reference_at_runtime() {
    // Parity with the reference: `let a = 5` / `-2` folds into `5 - 2`
    // (the reference prints 3 for this program; verified by running it).
    let output =
        run_source("@Main function main() {\n    let a = 5\n    -2\n    print(a)\n    return\n}");
    assert!(output.status.success());
    assert_eq!(String::from_utf8_lossy(&output.stdout), "3\n");
}

/// Writes an entry program plus the modules it imports into one directory, and
/// returns the entry path. A dotted module name is a directory path.
fn write_program(entry: &str, modules: &[(&str, &str)]) -> PathBuf {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    let directory = std::env::temp_dir().join(format!("kirac_e2e_program_{pid}_{unique}"));
    std::fs::create_dir_all(&directory).expect("temp dir");
    for (name, text) in modules {
        let module = directory.join(format!("{name}.kira"));
        if let Some(parent) = module.parent() {
            std::fs::create_dir_all(parent).expect("module directory");
        }
        std::fs::write(&module, text).expect("write module");
    }
    let path = directory.join("main.kira");
    std::fs::write(&path, entry).expect("write entry");
    path
}

/// `kirac` resolves an import against the entry file's directory, so a program
/// spread over several files runs from the real binary the way a user runs it.
#[test]
fn runs_a_program_spread_across_modules() {
    let path = write_program(
        "import geometry as Geo\nimport shapes.Rect as Rect\n\
         @Main function main() { let p: Geo.Point = Point { x: 3, y: 4 } \
         print(p.manhattan()) print(Rect.area(p)) return }",
        &[
            (
                "geometry",
                "struct Point { let x: Int  let y: Int\n\
                 function manhattan() -> Int { return x + y } }",
            ),
            (
                "shapes/Rect",
                "function area(p: borrow Point) -> Int { return p.x * p.y }",
            ),
        ],
    );
    let output = kirac(&["run", path.to_str().unwrap()]);
    let _ = std::fs::remove_dir_all(path.parent().expect("program directory"));
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout), "7\n12\n");
}

/// A diamond import graph through the real binary: the entry imports `a` and
/// `b`, `b` imports `a` and holds one of its structs in a field.
///
/// The entry deliberately names `a` first, which is the order a depth-first
/// *pre-order* walk gets wrong: it records `a` then `b`, and the reverse that
/// followed put `b`'s items ahead of `a`'s, rejecting `struct BBox` with a
/// KSEM051 telling the author to move a struct that lives in another file.
/// Dependencies-first is a property of the graph, not of the entry file's
/// typing order.
#[test]
fn runs_a_diamond_import_graph() {
    let path = write_program(
        "import a\nimport b\n\
         @Main function main() { \
         let boxed = BBox { corner: APoint { x: 1, y: 2 } } \
         print(bValue() + boxed.corner.x - 1) return }",
        &[
            (
                "a",
                "struct APoint { let x: Int  let y: Int }\n\
                 function aValue() -> Int { return 3 }",
            ),
            (
                "b",
                "import a\nstruct BBox { let corner: APoint }\n\
                 function bValue() -> Int { return aValue() + 4 }",
            ),
        ],
    );
    let output = kirac(&["run", path.to_str().unwrap()]);
    let _ = std::fs::remove_dir_all(path.parent().expect("program directory"));
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout), "7\n");
}

/// An import that names no file on disk is a compile error, and the message
/// names the module and where the compiler looked.
#[test]
fn an_import_of_a_missing_module_is_rejected() {
    let path = write_program(
        "import nowhere\n@Main function main() { print(1) return }",
        &[],
    );
    let output = kirac(&["check", path.to_str().unwrap()]);
    let _ = std::fs::remove_dir_all(path.parent().expect("program directory"));
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("KSEM032"), "{stderr}");
    assert!(stderr.contains("nowhere"), "{stderr}");
}

/// A diagnostic raised inside an imported module is rendered against *that*
/// file — the header names the module, not the entry program.
#[test]
fn a_modules_diagnostic_renders_against_the_module_file() {
    let path = write_program(
        "import broken\n@Main function main() { print(1) return }",
        &[("broken", "function bad() -> Int { return nope }")],
    );
    let output = kirac(&["check", path.to_str().unwrap()]);
    let _ = std::fs::remove_dir_all(path.parent().expect("program directory"));
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("KSEM060"), "{stderr}");
    assert!(
        stderr.contains("broken.kira"),
        "the module's own path is what the error points at: {stderr}"
    );
}

/// Imports are file-scoped: the entry file's import does not put a namespace
/// root into a sibling module.
#[test]
fn a_siblings_import_does_not_carry_into_a_module() {
    let path = write_program(
        "import support\nimport leak\n@Main function main() { print(leakValue()) return }",
        &[
            ("support", "function supportValue() -> Int { return 1 }"),
            (
                "leak",
                "function leakValue() -> Int { return support.supportValue() }",
            ),
        ],
    );
    let output = kirac(&["check", path.to_str().unwrap()]);
    let _ = std::fs::remove_dir_all(path.parent().expect("program directory"));
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("KSEM027"), "{stderr}");
}

// ---------------------------------------------------------------------------
// Library packages
//
// `kind = .Library` in `package.kira` is what makes a package a library, and
// these prove the three things that follow from it end to end, through the real
// binary: a library with no `@Main` checks clean, running one is refused by
// name, and building one produces an artifact on each backend the CI machine
// has.
// ---------------------------------------------------------------------------

/// Writes a package directory with a `package.kira` and one source file, and
/// returns the source path.
fn write_package(kind: &str, source: &str) -> PathBuf {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    let directory = std::env::temp_dir().join(format!("kirac_e2e_pkg_{pid}_{unique}"));
    std::fs::create_dir_all(&directory).expect("temp dir");
    std::fs::write(
        directory.join("package.kira"),
        format!(
            "Package uifoundation {{\n    let version = \"0.1.0\"\n    let kind = {kind}\n}}\n"
        ),
    )
    .expect("write package.kira");
    let path = directory.join("uifoundation.kira");
    std::fs::write(&path, source).expect("write source");
    path
}

/// A library with no entrypoint: the thing that could not be written before.
const LIBRARY_SOURCE: &str = "function add(a: Int, b: Int) -> Int { return a + b }\n\
     function greeting(name: String) -> String { return \"hello \" + name }";

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

// ---------------------------------------------------------------------------
// The `@Export` surface
//
// Step 1 of the export feature is the frontend only: `@Export` parses, the
// package rule and the boundary refusals are checked, and names map to their
// consumer spelling. No engine serves an export yet, so a library that declares
// one is refused at `build` by name — on every backend, each with its own
// reason. `check` is the verb that works today, which is what these prove
// through the real binary.
// ---------------------------------------------------------------------------

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
    let output = kirac(&["check", path.to_str().unwrap()]);
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
    let output = kirac(&["check", path.to_str().unwrap()]);
    let _ = std::fs::remove_dir_all(path.parent().expect("package directory"));
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("KSEM159"), "{stderr}");
}

#[test]
fn every_backend_refuses_to_build_an_export_by_name_with_its_own_reason() {
    // The refusal names the backend and says what *that* engine still owes, so
    // a user knows which one they are waiting on rather than being told a bare
    // "not supported".
    let reasons = [
        ("vm", "KBC1 exports section"),
        ("llvm", "kira_lib_*"),
        ("hybrid", "neither half's export surface"),
    ];
    for (backend, reason) in reasons {
        let path = write_package(".Library", EXPORTING_LIBRARY);
        let output = kirac(&["build", "--backend", backend, path.to_str().unwrap()]);
        let _ = std::fs::remove_dir_all(path.parent().expect("package directory"));
        assert_eq!(
            output.status.code(),
            Some(1),
            "the {backend} backend built an export"
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("library export is not built yet"),
            "the {backend} backend refused for a different reason: {stderr}"
        );
        assert!(
            stderr.contains(&format!("`--backend {backend}`")),
            "the refusal did not name the backend: {stderr}"
        );
        assert!(
            stderr.contains(reason),
            "the {backend} reason was missing: {stderr}"
        );
        // The derived consumer names are listed, so the author can see what
        // surface the engine will eventually have to serve.
        assert!(
            stderr.contains("make_button, button_width, click_at"),
            "{stderr}"
        );
    }
}

#[test]
fn the_web_refuses_to_build_an_export_too() {
    let path = write_package(".Library", EXPORTING_LIBRARY);
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
        stderr.contains("library export is not built yet"),
        "{stderr}"
    );
    assert!(stderr.contains("`--device wasm32`"), "{stderr}");
    assert!(
        stderr.contains("string/allocator contract"),
        "the wasm reason was missing: {stderr}"
    );
}

#[test]
fn a_library_that_exports_nothing_still_builds() {
    // The refusal is scoped to a declared export, not to libraries: step 0's
    // artifact must keep working.
    let path = write_package(".Library", LIBRARY_SOURCE);
    let output = kirac(&["build", "--backend", "vm", path.to_str().unwrap()]);
    let _ = std::fs::remove_dir_all(path.parent().expect("package directory"));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stderr}");
}
