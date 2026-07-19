//! The bundled Foundation, driven through the real `kirac` binary.
//!
//! Everything else about this feature can be tested against a bundle a test
//! stood up itself. This module is the one that cannot: the programs here run
//! in a directory holding nothing but their own source, invoked through the
//! installed compiler, so an import that resolves proves the *shipped*
//! discovery worked — the binary found a Foundation without being told where
//! one was.

use crate::{kirac, write_program, write_source};

/// The mechanism, as a user meets it: an import with no path, no dependency
/// entry, and nothing beside the program on disk.
#[test]
fn runs_a_program_that_imports_the_bundled_foundation() {
    let path = write_source(
        "import Foundation\n\
         @Main function main() { printLine(\"hello from Foundation\") return }",
    );
    let output = kirac(&["run", path.to_str().expect("a utf-8 path")]);
    let _ = std::fs::remove_file(&path);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "hello from Foundation\n"
    );
}

/// The import binds a namespace root too, so Foundation is reachable by the
/// same two spellings any imported module is.
#[test]
fn calls_the_bundled_foundation_through_its_namespace_root() {
    let path = write_source(
        "import Foundation\n\
         @Main function main() { Foundation.printLine(\"qualified\") return }",
    );
    let output = kirac(&["run", path.to_str().expect("a utf-8 path")]);
    let _ = std::fs::remove_file(&path);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout), "qualified\n");
}

/// Foundation is imported, never implicit. A file that does not import it does
/// not get it — which is what makes the passing cases above statements about
/// the import rather than about the compiler injecting a prelude.
#[test]
fn foundation_is_not_available_without_an_import() {
    let path = write_source("@Main function main() { printLine(\"x\") return }");
    let output = kirac(&["check", path.to_str().expect("a utf-8 path")]);
    let _ = std::fs::remove_file(&path);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("KSEM061"), "{stderr}");
}

/// The import is file-scoped, exactly as an import of a project's own module
/// is: a sibling that wants Foundation's namespace root writes its own import,
/// and one that does not gets KSEM027 telling it so.
#[test]
fn a_file_that_did_not_import_foundation_cannot_name_its_root() {
    let path = write_program(
        "import Foundation\n@Main function main() { helper() return }",
        &[(
            "support",
            "function helper() { Foundation.printLine(\"x\") return }",
        )],
    );
    // `support` is only part of the program because the entry imports it.
    let entry = path.parent().expect("program directory").join("main.kira");
    std::fs::write(
        &entry,
        "import Foundation\nimport support\n@Main function main() { helper() return }",
    )
    .expect("rewrite entry");
    let output = kirac(&["check", entry.to_str().expect("a utf-8 path")]);
    let _ = std::fs::remove_dir_all(path.parent().expect("program directory"));
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("KSEM027"), "{stderr}");
}

/// A program that ships its own `Foundation.kira` gets that one. The toolchain
/// never reaches into a program to replace a file its author wrote, so
/// installing a new Foundation cannot change what such a program means.
#[test]
fn a_projects_own_foundation_shadows_the_bundled_one() {
    let path = write_program(
        "import Foundation\n@Main function main() { printLine(\"x\") return }",
        &[(
            "Foundation",
            "function printLine(text: borrow String) { print(\"local: \" + text) return }",
        )],
    );
    let output = kirac(&["run", path.to_str().expect("a utf-8 path")]);
    let _ = std::fs::remove_dir_all(path.parent().expect("program directory"));
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout), "local: x\n");
}

/// Declaring a name Foundation also declares is the ordinary duplicate, not a
/// special case: the bundle's declarations are the program's declarations once
/// it is imported, so the collision is reported by name where it is written.
#[test]
fn a_name_foundation_also_declares_is_an_ordinary_duplicate() {
    let path = write_source(
        "import Foundation\n\
         function printLine(text: borrow String) { print(\"mine\") return }\n\
         @Main function main() { printLine(\"x\") return }",
    );
    let output = kirac(&["check", path.to_str().expect("a utf-8 path")]);
    let _ = std::fs::remove_file(&path);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("KSEM003"), "{stderr}");
    assert!(stderr.contains("printLine"), "{stderr}");
}
