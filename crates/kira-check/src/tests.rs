//! What a check answers, proven against this checkout's own Foundation.
//!
//! Every case here builds its package set in memory and asserts on codes and
//! files rather than on messages — the same discipline the surface exists to
//! give a Kira caller.

use super::*;
use kira_runtime_abi::{CheckFile, CheckPackage, CheckSeverity};

/// The Foundation in this checkout, as a bundled root.
///
/// Pinned rather than discovered: discovery prefers whatever toolchain the
/// machine has installed, and these tests are about the Foundation in the tree.
fn foundation() -> Vec<BundledRoot> {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../foundation");
    vec![BundledRoot::new("Foundation", root.join("app"))]
}

fn session() -> CheckSession {
    CheckSession::with_bundles(foundation())
}

fn app(files: &[(&str, &str)]) -> CheckPackage {
    package("Package App {\n    let kind = .App\n}\n", files)
}

fn package(manifest: &str, files: &[(&str, &str)]) -> CheckPackage {
    CheckPackage {
        manifest: manifest.to_owned(),
        files: files
            .iter()
            .map(|&(path, text)| CheckFile {
                path: path.to_owned(),
                text: text.to_owned(),
            })
            .collect(),
    }
}

fn request(root: &str, packages: Vec<CheckPackage>) -> CheckRequest {
    CheckRequest {
        root: root.to_owned(),
        packages,
    }
}

/// Every error, as `(code, file)` pairs — the two things an assertion is about.
fn errors(diagnostics: &[CheckDiagnostic]) -> Vec<(&str, &str)> {
    diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.severity == CheckSeverity::Error)
        .map(|diagnostic| (diagnostic.code.as_str(), diagnostic.file.as_str()))
        .collect()
}

#[test]
fn a_clean_package_produces_no_diagnostics() {
    let mut session = session();
    let diagnostics = session.check(&request(
        "App",
        vec![app(&[(
            "app/main.kira",
            "import Foundation\n@Main function main() { printLine(\"hello\") return }",
        )])],
    ));
    assert!(errors(&diagnostics).is_empty(), "{diagnostics:?}");
}

#[test]
fn an_error_names_its_code_and_the_file_it_is_in() {
    let mut session = session();
    let diagnostics = session.check(&request(
        "App",
        vec![app(&[
            ("app/main.kira", "@Main function main() { helper() return }"),
            (
                "app/Helper.kira",
                "function helper() -> Int { return notDeclaredAnywhere }",
            ),
        ])],
    ));
    assert_eq!(errors(&diagnostics), vec![("KSEM060", "app/Helper.kira")]);
}

/// The rule a single-source API cannot express: an import binds the file it was
/// written in, and no other file of the same package.
#[test]
fn an_import_in_one_file_is_not_visible_in_another() {
    let mut session = session();
    let diagnostics = session.check(&request(
        "App",
        vec![app(&[
            (
                "app/main.kira",
                "import Foundation\n@Main function main() { printLine(\"a\") return }",
            ),
            (
                "app/Other.kira",
                "function other() { printLine(\"b\") return }",
            ),
        ])],
    ));
    assert_eq!(errors(&diagnostics), vec![("KSEM061", "app/Other.kira")]);
}

/// The same two files, with the import written in both, compile.
///
/// The other half of the rule: what is refused above is refused for the reason
/// stated, not because the sibling could never see Foundation.
#[test]
fn the_same_import_written_in_both_files_compiles() {
    let mut session = session();
    let diagnostics = session.check(&request(
        "App",
        vec![app(&[
            (
                "app/main.kira",
                "import Foundation\n@Main function main() { other() return }",
            ),
            (
                "app/Other.kira",
                "import Foundation\nfunction other() { printLine(\"b\") return }",
            ),
        ])],
    ));
    assert!(errors(&diagnostics).is_empty(), "{diagnostics:?}");
}

/// A package is one flat namespace, so two of its files declaring one name
/// collide — and the collision is reported in the file that declared it second.
#[test]
fn two_files_of_one_package_may_not_declare_the_same_name() {
    let mut session = session();
    let diagnostics = session.check(&request(
        "App",
        vec![app(&[
            ("app/main.kira", "@Main function main() { return }"),
            ("app/A.kira", "function shared() -> Int { return 1 }"),
            ("app/B.kira", "function shared() -> Int { return 2 }"),
        ])],
    ));
    let codes: Vec<&str> = errors(&diagnostics)
        .into_iter()
        .map(|(code, _)| code)
        .collect();
    assert!(
        codes.iter().any(|code| code.starts_with("KSEM")),
        "{diagnostics:?}"
    );
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.file == "app/B.kira"),
        "{diagnostics:?}"
    );
}

/// A library plus the app on top of it: two packages, one edge.
#[test]
fn an_app_may_import_a_library_package_in_the_same_request() {
    let mut session = session();
    let core = package(
        "Package Core {\n    let kind = .Library\n    let moduleRoot = \"Core\"\n}\n",
        &[("app/Core.kira", "function coreValue() -> Int { return 41 }")],
    );
    let diagnostics = session.check(&request(
        "App",
        vec![
            core,
            app(&[(
                "app/main.kira",
                "import Core\n@Main function main() { print(coreValue() + 1) return }",
            )]),
        ],
    ));
    assert!(errors(&diagnostics).is_empty(), "{diagnostics:?}");
}

/// Visibility does not compose and it does not leak: without the import, the
/// library's declaration is not nameable.
#[test]
fn a_library_package_is_invisible_without_the_import() {
    let mut session = session();
    let core = package(
        "Package Core {\n    let kind = .Library\n    let moduleRoot = \"Core\"\n}\n",
        &[("app/Core.kira", "function coreValue() -> Int { return 41 }")],
    );
    let diagnostics = session.check(&request(
        "App",
        vec![
            core,
            app(&[(
                "app/main.kira",
                "@Main function main() { print(coreValue()) return }",
            )]),
        ],
    ));
    assert_eq!(errors(&diagnostics), vec![("KSEM061", "app/main.kira")]);
}

/// A library root needs no `@Main`; the manifest is what says so.
#[test]
fn a_library_root_package_needs_no_main() {
    let mut session = session();
    let diagnostics = session.check(&request(
        "Core",
        vec![package(
            "Package Core {\n    let kind = .Library\n    let moduleRoot = \"Core\"\n}\n",
            &[("app/Core.kira", "function coreValue() -> Int { return 41 }")],
        )],
    ));
    assert!(errors(&diagnostics).is_empty(), "{diagnostics:?}");
}

/// Macro expansion is memoized per file, so the same file's bytes appearing in
/// two calls must still expand under *that* call's macros.
///
/// The entry file here is byte-identical across both calls and sits at the same
/// source id; only the macro beside it changes. An expansion cached on the
/// file's bytes alone would answer the second call with the first's expansion
/// and let a program type-check against a macro it never declared.
#[test]
fn a_files_expansion_follows_the_macros_of_the_call_it_is_in() {
    let mut session = session();
    let entry = "@Main function main() { let value: Int = pick!() return }";
    let integer = "macro pick() {\n    expand {\n        1\n    }\n}\n";
    let text = "macro pick() {\n    expand {\n        \"text\"\n    }\n}\n";

    let first = session.check(&request(
        "App",
        vec![app(&[
            ("app/main.kira", entry),
            ("app/Macros.kira", integer),
        ])],
    ));
    assert!(errors(&first).is_empty(), "{first:?}");

    let second = session.check(&request(
        "App",
        vec![app(&[("app/main.kira", entry), ("app/Macros.kira", text)])],
    ));
    assert!(
        !errors(&second).is_empty(),
        "the second call's `pick!()` expands to a String, so `let value: Int` \
         must not type-check: {second:?}"
    );

    // And back: the first program still answers cleanly through the same
    // session, so nothing was poisoned by the second.
    let again = session.check(&request(
        "App",
        vec![app(&[
            ("app/main.kira", entry),
            ("app/Macros.kira", integer),
        ])],
    ));
    assert!(errors(&again).is_empty(), "{again:?}");
}

/// One call may not see another's declarations, and two calls may declare the
/// same name. A session reuses what is shared and immutable and nothing else.
#[test]
fn two_calls_are_isolated_from_each_other() {
    let mut session = session();
    let first = session.check(&request(
        "App",
        vec![app(&[(
            "app/main.kira",
            "@Main function main() { return }\nfunction sameName() -> Int { return 1 }",
        )])],
    ));
    assert!(errors(&first).is_empty(), "{first:?}");

    let second = session.check(&request(
        "App",
        vec![app(&[(
            "app/main.kira",
            "@Main function main() { return }\nfunction sameName() -> Int { return 2 }",
        )])],
    ));
    assert!(errors(&second).is_empty(), "{second:?}");

    // And a name only the first call declared is not in scope for a third.
    let third = session.check(&request(
        "App",
        vec![app(&[(
            "app/main.kira",
            "@Main function main() { print(sameName()) return }",
        )])],
    ));
    assert_eq!(errors(&third), vec![("KSEM061", "app/main.kira")]);
}

/// Foundation is on disk and everything else is not; a check reads no file the
/// caller did not hand it.
#[test]
fn a_request_reads_no_file_of_its_own() {
    let mut session = session();
    let diagnostics = session.check(&request(
        "App",
        vec![app(&[(
            "app/main.kira",
            "import NotAPackageAnywhere\n@Main function main() { return }",
        )])],
    ));
    let codes: Vec<&str> = errors(&diagnostics)
        .into_iter()
        .map(|(code, _)| code)
        .collect();
    assert_eq!(codes, vec!["KSEM032"], "{diagnostics:?}");
}

#[test]
fn a_request_naming_no_root_answers_with_a_package_diagnostic() {
    let mut session = session();
    let diagnostics = session.check(&request("Nothing", Vec::new()));
    assert_eq!(errors(&diagnostics), vec![("KPK031", "")]);
}
