//! Foundation's toolchain — `Kira.check`, `Kira.build`, `Kira.runApp` — driven
//! from a Kira program.
//!
//! The sibling of [`super::compiler`], and where it differs is the whole point.
//! Checking a package set held in memory needs only the frontend, which links
//! into a native binary, so that surface answers the same on every backend.
//! Driving the toolchain over a project on a disk needs the build system, and a
//! standalone native binary does not carry one. So these cases prove two
//! things: under the `kira` host — the VM and the hybrid runtime — the verbs
//! answer with the program's own diagnostics and its exit code; and a
//! standalone native build refuses by name rather than answering as though the
//! package compiled.

use std::path::{Path, PathBuf};
use std::process::Output;
use std::sync::atomic::{AtomicU32, Ordering};

/// Writes the two packages the driver works on, and returns their directory.
///
/// One compiles and prints a line; the other calls an undefined name, which is
/// `KSEM060` on the line the call is written. The driver reads both by absolute
/// path, so the paths are baked into the driver this returns beside them.
fn workspace() -> (PathBuf, String) {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    let root = std::env::temp_dir().join(format!("kira_toolchain_{pid}_{unique}"));

    let good = root.join("good");
    std::fs::create_dir_all(good.join("app")).expect("good dir");
    std::fs::write(
        good.join("package.kira"),
        "Package Good {\n    let version = \"0.1.0\"\n    let kind = .App\n}\n",
    )
    .expect("good manifest");
    std::fs::write(
        good.join("app/main.kira"),
        "import Foundation\n\n@Main function main() {\n    printLine(\"from Good\")\n    return\n}\n",
    )
    .expect("good main");

    let bad = root.join("bad");
    std::fs::create_dir_all(bad.join("app")).expect("bad dir");
    std::fs::write(
        bad.join("package.kira"),
        "Package Bad {\n    let version = \"0.1.0\"\n    let kind = .App\n}\n",
    )
    .expect("bad manifest");
    std::fs::write(
        bad.join("app/main.kira"),
        "import Foundation\n\n@Main function main() {\n    printLine(missingName)\n    return\n}\n",
    )
    .expect("bad main");

    let driver = root.join("driver.kira");
    std::fs::write(&driver, driver_source(&good, &bad)).expect("driver");
    (root, driver.to_str().expect("utf-8 path").to_owned())
}

/// A path as the body of a Kira string literal.
///
/// A Windows path is full of backslashes, and a backslash begins an escape:
/// pasting `C:\Users\runneradmin\…` in raw makes `\U` and `\r` out of the
/// directory names, and the program fails to lex before any of this is
/// exercised. The two characters a literal cannot hold as themselves are
/// escaped, and nothing else is touched.
fn kira_string_literal(path: &Path) -> String {
    path.to_str()
        .expect("utf-8 path")
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
}

/// The driver program: it checks both packages, runs the good one, and prints a
/// line per fact a case asserts on.
fn driver_source(good: &Path, bad: &Path) -> String {
    let good = kira_string_literal(good);
    let bad = kira_string_literal(bad);
    format!(
        r#"import Foundation

@Main function main() {{
    let kira = Kira()

    let goodCheck = kira.check("{good}", .Vm)
    printLine("good errors: " + String(errorCount(goodCheck.diagnostics)))

    let badCheck = kira.check("{bad}", .Vm)
    printLine("bad errors: " + String(errorCount(badCheck.diagnostics)))
    printLine("bad KSEM060: " + String(firstIsKsem060(badCheck.diagnostics)))
    printLine("bad line: " + String(firstLine(badCheck.diagnostics)))

    let goodRun = kira.runApp("{good}", .Vm, [])
    printLine("run exit: " + String(goodRun.exitCode))
    return
}}

function errorCount(diagnostics: borrow [KiraDiagnostic]) -> Int {{
    var count = 0
    var index = 0
    while index < diagnostics.count {{
        if diagnostics[index].severity == .Error {{
            count = count + 1
        }}
        index = index + 1
    }}
    return count
}}

function firstIsKsem060(diagnostics: borrow [KiraDiagnostic]) -> Bool {{
    if diagnostics.count == 0 {{
        return false
    }}
    return diagnostics[0].code == .KSEM060
}}

function firstLine(diagnostics: borrow [KiraDiagnostic]) -> Int {{
    if diagnostics.count == 0 {{
        return 0
    }}
    return diagnostics[0].line
}}
"#
    )
}

/// Runs the driver on `backend` and hands back what the process did.
fn run_driver(driver: &str, backend: &str) -> Output {
    crate::run_on(Path::new(driver), backend)
}

/// Every fact the driver prints, so a host-backed backend can be held to it.
const EXPECTED: &str = "good errors: 0\n\
     bad errors: 1\n\
     bad KSEM060: true\n\
     bad line: 4\n\
     from Good\n\
     run exit: 0\n";

/// On the VM, the toolchain answers with each package's own diagnostics, names
/// the failing package's code and line, and runs the clean one to a zero exit.
#[test]
fn the_toolchain_answers_by_value_on_the_vm() {
    let (root, driver) = workspace();
    let output = run_driver(&driver, "vm");
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let _ = std::fs::remove_dir_all(&root);
    assert_eq!(
        stdout,
        EXPECTED,
        "vm stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// The hybrid runtime reaches the same installed toolchain from its native
/// half, so a program that drives the toolchain answers identically there.
#[test]
fn the_toolchain_answers_by_value_on_hybrid() {
    let (root, driver) = workspace();
    let output = run_driver(&driver, "hybrid");
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let _ = std::fs::remove_dir_all(&root);
    assert_eq!(
        stdout,
        EXPECTED,
        "hybrid stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// A standalone native build carries no build system, so it refuses the
/// toolchain by name and stops — it never answers as though the package
/// compiled, which an empty diagnostic list would read as.
#[test]
fn a_standalone_native_build_refuses_the_toolchain_by_name() {
    let (root, driver) = workspace();
    let output = run_driver(&driver, "llvm");
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let _ = std::fs::remove_dir_all(&root);
    assert_ne!(output.status.code(), Some(0), "native must not exit clean");
    assert!(
        stderr.contains("does not provide a toolchain"),
        "native refusal must name the missing capability; stderr: {stderr}, stdout: {stdout}"
    );
}
