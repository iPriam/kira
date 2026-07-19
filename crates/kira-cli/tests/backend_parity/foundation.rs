//! The bundled Foundation on every backend.
//!
//! `import Foundation` resolves in the *loader*, not in any backend: by the
//! time the IR exists, Foundation's functions are functions like any others and
//! nothing downstream can tell they came from a package installed beside the
//! compiler rather than from a file beside the program. These cases are what
//! turns that claim into a result — the answer comes out of Foundation, and the
//! three backends have to agree on it.
//!
//! They also pin the mechanism itself: these programs are written in a
//! temporary directory that holds nothing but the entry file, so an import that
//! resolved would have to have found Foundation through the toolchain.

use crate::{assert_module_parity, assert_parity};

/// The whole mechanism in one program: an import with no path, no dependency
/// entry, and nothing beside it on disk.
#[test]
fn a_bundled_foundation_call_runs_the_same_on_every_backend() {
    let out = assert_parity(
        "import Foundation\n\
         @Main function main() { printLine(\"from Foundation\") return }",
    );
    assert_eq!(out, "from Foundation\n");
}

/// The import binds a namespace root as well as making the declarations
/// callable bare, exactly as an import of a project's own module does.
#[test]
fn a_qualified_foundation_call_runs_the_same_on_every_backend() {
    let out = assert_parity(
        "import Foundation\n\
         @Main function main() { Foundation.printLine(\"qualified\") return }",
    );
    assert_eq!(out, "qualified\n");
}

/// A module of the program's own may import Foundation too — the bundle is
/// reached from anywhere in the graph, not only from the entry file.
#[test]
fn a_module_may_import_foundation_on_every_backend() {
    let out = assert_module_parity(
        "import support\n@Main function main() { greet() return }",
        &[(
            "support",
            "import Foundation\nfunction greet() { printLine(\"greetings\") return }",
        )],
    );
    assert_eq!(out, "greetings\n");
}

/// The project always wins. A `Foundation.kira` the author wrote beside their
/// program is the one that is loaded, so installing a toolchain can never
/// change what a program that shipped its own module by that name means.
#[test]
fn a_projects_own_foundation_shadows_the_bundled_one_on_every_backend() {
    let out = assert_module_parity(
        "import Foundation\n@Main function main() { printLine(\"x\") return }",
        &[(
            "Foundation",
            "function printLine(text: borrow String) { print(\"local: \" + text) return }",
        )],
    );
    assert_eq!(out, "local: x\n");
}

/// Foundation is imported, never implicit: a file that does not import it
/// cannot call into it, and every backend refuses the same way. This is the
/// negative half of the mechanism — without it, a passing positive case could
/// equally mean Foundation had been injected into every program.
#[test]
fn foundation_is_not_implicitly_available_on_any_backend() {
    let out = assert_parity("@Main function main() { printLine(\"x\") return }");
    assert_eq!(
        out, "",
        "an unimported Foundation must not resolve: {out:?}"
    );
}
