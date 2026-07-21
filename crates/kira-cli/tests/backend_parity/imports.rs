//! Imports on every backend.
//!
//! An import is resolved entirely in the frontend: by the time the IR exists, a
//! multi-file program is one flat list of functions and nothing downstream can
//! tell it was ever more than one file. So there is no opcode, no runtime
//! helper, and no lowering to get wrong — which is exactly the claim these
//! cases make, by running programs whose *answers* come from imported modules
//! and requiring the three backends to agree on them.

use crate::assert_module_parity;

/// A function declared in a module, called from the entry file.
#[test]
fn a_module_function_runs_the_same_on_every_backend() {
    let out = assert_module_parity(
        "import support\n@Main function main() { print(supportValue()) return }",
        &[("support", "function supportValue() -> Int { return 42 }")],
    );
    assert_eq!(out, "42\n");
}

/// The qualified spelling compiles to the same call as the bare one.
#[test]
fn a_qualified_call_runs_the_same_on_every_backend() {
    let out = assert_module_parity(
        "import support as Support\n\
         @Main function main() { print(Support.supportValue()) print(supportValue()) return }",
        &[("support", "function supportValue() -> Int { return 7 }")],
    );
    assert_eq!(out, "7\n7\n");
}

/// A struct declared in a module, constructed and used in the entry file —
/// including through its module-qualified type name.
#[test]
fn a_module_struct_runs_the_same_on_every_backend() {
    let out = assert_module_parity(
        "import geometry as Geo\n\
         @Main function main() { \
         let p: Geo.Point = Point { x: 3, y: 4 } print(p.area()) return }",
        &[(
            "geometry",
            "struct Point { let x: Int  let y: Int\n\
             function area() -> Int { return x * y } }",
        )],
    );
    assert_eq!(out, "12\n");
}

/// A module-qualified struct literal, a module-qualified type, a
/// module-qualified enum variant (payload-less and payload-carrying), and a
/// module-qualified call, all in one program: the qualified spellings compile
/// to the same constructions and calls the bare forms do, so the three backends
/// agree on the answer.
#[test]
fn qualified_cross_module_references_run_the_same_on_every_backend() {
    let out = assert_module_parity(
        "import shapes as S\n\
         @Main function main() { \
         let b: S.Box = S.Box { size: S.Size.Large, tag: S.Size.Coded(7) } \
         print(S.score(b)) print(score(b)) return }",
        &[(
            "shapes",
            "enum Size { Small  Large  Coded(Int) }\n\
             struct Box { let size: Size  let tag: Size }\n\
             function score(b: borrow Box) -> Int { \
             let base = b.size == .Large ? 10 : 1  return base + coded(b.tag) }\n\
             function coded(s: borrow Size) -> Int { if s == .Small { return 0 } return 5 }",
        )],
    );
    assert_eq!(out, "15\n15\n");
}

/// A module importing a module: the graph is transitive, and the deepest
/// dependency's declarations are available to the ones above it.
#[test]
fn a_transitive_module_runs_the_same_on_every_backend() {
    let out = assert_module_parity(
        "import middle\n@Main function main() { print(middleValue()) return }",
        &[
            ("base", "function baseValue() -> Int { return 10 }"),
            (
                "middle",
                "import base\nfunction middleValue() -> Int { return baseValue() + 5 }",
            ),
        ],
    );
    assert_eq!(out, "15\n");
}

/// A dotted module path is a directory path on disk, and it runs like any
/// other module.
#[test]
fn a_dotted_module_runs_the_same_on_every_backend() {
    let out = assert_module_parity(
        "import Foundation.Web as Web\n@Main function main() { print(Web.webValue()) return }",
        &[("Foundation/Web", "function webValue() -> Int { return 99 }")],
    );
    assert_eq!(out, "99\n");
}

/// An enum declared in a module, used from the entry file: the module's types
/// reach every backend the same way its functions do.
#[test]
fn a_module_enum_runs_the_same_on_every_backend() {
    let out = assert_module_parity(
        "import palette\n\
         @Main function main() { print(rank(.Green)) print(rank(.Red)) return }",
        &[(
            "palette",
            "enum Color { Red Green Blue }\n\
             function rank(c: borrow Color) -> Int { if c == .Red { return 1 } return 2 }",
        )],
    );
    assert_eq!(out, "2\n1\n");
}

/// Two modules that import each other are a legal program — the loader is
/// visited-set guarded — and they run identically on all three backends.
#[test]
fn mutually_importing_modules_run_the_same_on_every_backend() {
    let out = assert_module_parity(
        "import alpha\n@Main function main() { print(alphaValue()) return }",
        &[
            (
                "alpha",
                "import beta\nfunction alphaBase() -> Int { return 3 }\n\
                 function alphaValue() -> Int { return betaValue() + 1 }",
            ),
            (
                "beta",
                "import alpha\nfunction betaValue() -> Int { return alphaBase() * 2 }",
            ),
        ],
    );
    assert_eq!(out, "7\n");
}

/// A diamond: the entry imports `a` and `b`, and `b` also imports `a` and
/// names a struct `a` declares.
///
/// The shape exists to pin the module *order*, not the arithmetic. The walk is
/// depth-first post-order, so `a` is recorded before `b` because `b` imports
/// it — never because of where the entry file listed either one.
///
/// A pre-order walk gets this wrong in exactly one direction, which is the one
/// spelled here: with `a` listed first it records `a` then `b`, and the final
/// reverse puts `b`'s items ahead of `a`'s, so `struct BBox` is rejected for
/// holding a type "declared later".
#[test]
fn a_diamond_import_graph_runs_the_same_on_every_backend() {
    let out = assert_module_parity(
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
    assert_eq!(out, "7\n");
}

/// The same diamond with the entry's imports the other way round: which order
/// the entry file lists independent imports in decides nothing about whether a
/// sibling module compiles.
#[test]
fn a_diamonds_entry_import_order_does_not_decide_the_program() {
    let out = assert_module_parity(
        "import b\nimport a\n\
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
    assert_eq!(out, "7\n");
}

/// An `@Runtime`/`@Native` split across module boundaries: the hybrid backend
/// builds a real execution boundary out of functions that were written in
/// different files, and still agrees with the two single-engine backends.
#[test]
fn an_execution_boundary_crosses_a_module_boundary() {
    let out = assert_module_parity(
        "import worker\n@Main @Runtime function main() { print(nativeDouble(21)) return }",
        &[(
            "worker",
            "@Native function nativeDouble(n: Int) -> Int { return n * 2 }",
        )],
    );
    assert_eq!(out, "42\n");
}
