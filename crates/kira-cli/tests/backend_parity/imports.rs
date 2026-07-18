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
