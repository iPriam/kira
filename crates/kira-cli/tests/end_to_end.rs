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
    // A `class` is outside the v0 subset: it must not crash the compiler, and
    // it must be reported as not-yet-supported rather than silently ignored.
    let output = run_source("class Mode { }\n@Main function main() { print(1) return }");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("KSEM900"), "stderr was: {stderr}");
    assert!(stderr.contains("not supported yet"));
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
