//! Foundation's compiler on every backend.
//!
//! One compiler, reached two ways: the VM describes the request and hands it to
//! its host, while native code calls `kira_rt_compiler_check_packages`. Both go
//! through one `kira-check` session, and these cases are what turns that into a
//! result rather than a claim — including the hybrid case, where the two
//! engines run in one process.
//!
//! Every case asserts on a *code* and a *file*, never on a message. That is the
//! whole point of the surface: a message gets reworded and a code does not, and
//! a two-file case has to be able to say which of its files the compiler
//! objected to.

use crate::assert_parity;

/// The preamble every case here shares: a helper that builds one package.
///
/// Written once rather than in each case because the interesting part of a case
/// is its files, and repeating the construction would bury that.
const PRELUDE: &str = r#"
import Foundation

function sourceFile(path: borrow String, text: borrow String) -> KiraSourceFile {
    var file = KiraSourceFile()
    file.path = path
    file.text = text
    return file
}

function package(manifest: borrow String, files: borrow [KiraSourceFile]) -> KiraPackage {
    var built = KiraPackage()
    built.manifest = manifest
    built.files = files
    return built
}

function appManifest() -> String {
    return "Package App {\n    let kind = .App\n}\n"
}

function coreManifest() -> String {
    return "Package Core {\n    let kind = .Library\n    let moduleRoot = \"Core\"\n}\n"
}
"#;

/// Builds a program out of the shared prelude and a `@Main` body.
fn program(body: &str) -> String {
    format!("{PRELUDE}\n@Main\nfunction main() {{\n{body}\n    return\n}}\n")
}

/// A package that compiles clean says so, and reports nothing.
#[test]
fn a_clean_package_checks_on_every_backend() {
    let output = assert_parity(&program(
        r#"
    var files: [KiraSourceFile] = []
    files.append(sourceFile("app/main.kira", "import Foundation\n@Main function main() { printLine(\"hi\") return }"))
    let result = checkPackage("App", package(appManifest(), files))
    print(result.ok())
    print(result.errorCount())"#,
    ));
    assert_eq!(output, "true\n0\n");
}

/// A deliberate error comes back as a code, a severity, and the file it is in.
#[test]
fn an_error_names_its_code_and_file_on_every_backend() {
    let output = assert_parity(&program(
        r#"
    var files: [KiraSourceFile] = []
    files.append(sourceFile("app/main.kira", "@Main function main() { helper() return }"))
    files.append(sourceFile("app/Helper.kira", "function helper() -> Int { return notDeclaredAnywhere }"))
    let result = checkPackage("App", package(appManifest(), files))
    print(result.ok())
    print(result.errorCount())
    print(result.has(.KSEM060, "app/Helper.kira"))
    print(result.has(.KSEM060, "app/main.kira"))
    print(result.at(0).codeText)
    print(result.at(0).severity == .Error)"#,
    ));
    assert_eq!(output, "false\n1\ntrue\nfalse\nKSEM060\ntrue\n");
}

/// The rule no single-source API can express: an import binds the file it was
/// written in and no other file of the same package.
#[test]
fn an_import_in_one_file_is_not_visible_in_another_on_every_backend() {
    let output = assert_parity(&program(
        r#"
    var files: [KiraSourceFile] = []
    files.append(sourceFile("app/main.kira", "import Foundation\n@Main function main() { printLine(\"a\") return }"))
    files.append(sourceFile("app/Other.kira", "function other() { printLine(\"b\") return }"))
    let result = checkPackage("App", package(appManifest(), files))
    print(result.has(.KSEM061, "app/Other.kira"))

    var both: [KiraSourceFile] = []
    both.append(sourceFile("app/main.kira", "import Foundation\n@Main function main() { other() return }"))
    both.append(sourceFile("app/Other.kira", "import Foundation\nfunction other() { printLine(\"b\") return }"))
    print(checkPackage("App", package(appManifest(), both)).ok())"#,
    ));
    assert_eq!(output, "true\ntrue\n");
}

/// A package is one flat namespace, so two of its files may not declare one
/// name — and the second declaration is the one reported.
#[test]
fn two_files_of_one_package_collide_on_every_backend() {
    let output = assert_parity(&program(
        r#"
    var files: [KiraSourceFile] = []
    files.append(sourceFile("app/main.kira", "@Main function main() { return }"))
    files.append(sourceFile("app/A.kira", "function shared() -> Int { return 1 }"))
    files.append(sourceFile("app/B.kira", "function shared() -> Int { return 2 }"))
    let result = checkPackage("App", package(appManifest(), files))
    print(result.ok())
    print(result.at(0).file)"#,
    ));
    assert_eq!(output, "false\napp/B.kira\n");
}

/// Two packages with an edge between them: the app imports the library.
#[test]
fn an_app_imports_a_library_package_on_every_backend() {
    let output = assert_parity(&program(
        r#"
    var coreFiles: [KiraSourceFile] = []
    coreFiles.append(sourceFile("app/Core.kira", "function coreValue() -> Int { return 41 }"))

    var appFiles: [KiraSourceFile] = []
    appFiles.append(sourceFile("app/main.kira", "import Core\n@Main function main() { print(coreValue() + 1) return }"))

    var request = KiraCheckRequest()
    request.root = "App"
    request.packages.append(package(coreManifest(), coreFiles))
    request.packages.append(package(appManifest(), appFiles))
    print(checkPackages(request).ok())

    var withoutImport: [KiraSourceFile] = []
    withoutImport.append(sourceFile("app/main.kira", "@Main function main() { print(coreValue()) return }"))
    var refused = KiraCheckRequest()
    refused.root = "App"
    refused.packages.append(package(coreManifest(), coreFiles))
    refused.packages.append(package(appManifest(), withoutImport))
    print(checkPackages(refused).has(.KSEM061, "app/main.kira"))"#,
    ));
    assert_eq!(output, "true\ntrue\n");
}

/// A request that names a root no package declares answers with the package
/// diagnostic saying so, rather than with silence.
#[test]
fn an_unknown_root_is_reported_on_every_backend() {
    let output = assert_parity(&program(
        r#"
    var request = KiraCheckRequest()
    request.root = "Missing"
    let result = checkPackages(request)
    print(result.ok())
    print(result.hasCode(.KPK031))
    print(result.at(0).file)"#,
    ));
    assert_eq!(output, "false\ntrue\n\n");
}

/// Two checks in one program do not leak into each other: the same top-level
/// name declared by two different packages is two declarations, not a clash.
#[test]
fn two_checks_are_isolated_on_every_backend() {
    let output = assert_parity(&program(
        r#"
    var first: [KiraSourceFile] = []
    first.append(sourceFile("app/main.kira", "@Main function main() { return }\nfunction sameName() -> Int { return 1 }"))
    print(checkPackage("App", package(appManifest(), first)).ok())

    var second: [KiraSourceFile] = []
    second.append(sourceFile("app/main.kira", "@Main function main() { return }\nfunction sameName() -> Int { return 2 }"))
    print(checkPackage("App", package(appManifest(), second)).ok())

    var third: [KiraSourceFile] = []
    third.append(sourceFile("app/main.kira", "@Main function main() { print(sameName()) return }"))
    print(checkPackage("App", package(appManifest(), third)).has(.KSEM061, "app/main.kira"))"#,
    ));
    assert_eq!(output, "true\ntrue\ntrue\n");
}

/// A checked package reaches no filesystem of its own: an import that names no
/// package in the request and no bundled package resolves to nothing, whatever
/// happens to sit in the process's working directory.
#[test]
fn a_checked_package_reads_no_file_on_every_backend() {
    let output = assert_parity(&program(
        r#"
    var files: [KiraSourceFile] = []
    files.append(sourceFile("app/main.kira", "import NotAPackageAnywhere\n@Main function main() { return }"))
    let result = checkPackage("App", package(appManifest(), files))
    print(result.has(.KSEM032, "app/main.kira"))"#,
    ));
    assert_eq!(output, "true\n");
}
