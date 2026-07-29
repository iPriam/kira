//! One program spread over several files, driven through the real binary:
//! import resolution, the order the graph is typed in, and where a module's
//! diagnostic points.

use crate::{kira, write_program};

/// `kira` resolves an import against the entry file's directory, so a program
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
    let output = kira(&["run", path.to_str().unwrap()]);
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
    let output = kira(&["run", path.to_str().unwrap()]);
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
    let output = kira(&["check", path.to_str().unwrap()]);
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
    let output = kira(&["check", path.to_str().unwrap()]);
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
    let output = kira(&["check", path.to_str().unwrap()]);
    let _ = std::fs::remove_dir_all(path.parent().expect("program directory"));
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("KSEM027"), "{stderr}");
}
