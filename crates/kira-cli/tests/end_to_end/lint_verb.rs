//! `kira lint`, driven through the real binary.
//!
//! What these prove is that the whole chain holds: a `linter.kira` at a
//! package root is compiled with the package, Foundation's `LintRunner`
//! collector reads the `Lint` entries out of it, and what the entries ask for
//! is what gets reported. Nothing in the compiler names `Lint` or `KLINT003`,
//! so a break anywhere in that chain shows up here as a run that reports
//! nothing — which is why these assert on the finding rather than on the exit
//! status.

use std::path::{Path, PathBuf};

/// Builds a package in a fresh temp directory and returns its root.
///
/// `files` is `(relative path, contents)`. Directories are created as needed.
fn write_package(name: &str, files: &[(&str, &str)]) -> PathBuf {
    static COUNTER: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let unique = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!("kira_lint_{}_{unique}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("app")).expect("create the package");
    std::fs::write(
        root.join("package.kira"),
        format!(
            "Package {name} {{\n\
             \x20   let version = \"0.1.0\"\n\
             \x20   let kira = \"0.1.0\"\n\
             \x20   let kind = PackageKind.App\n\
             \x20   let defaults = Defaults {{ executionMode: Backend.Vm, buildTarget: BuildTarget.Host }}\n\
             }}\n"
        ),
    )
    .expect("write the manifest");
    for (relative, contents) in files {
        let path = root.join(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create a directory");
        }
        std::fs::write(&path, contents).expect("write a file");
    }
    root
}

/// Runs `kira lint` against the Foundation **in this checkout**.
///
/// Pinned for the same reason `tests_verb` pins it: these are about the runner
/// Foundation ships here, not about whichever toolchain was last installed.
fn kira_lint(root: &Path) -> std::process::Output {
    let foundation = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../foundation")
        .canonicalize()
        .expect("the checkout's foundation");
    std::process::Command::new(env!("CARGO_BIN_EXE_kira"))
        .env("KIRA_FOUNDATION_HOME", foundation)
        .args(["lint", root.to_str().expect("a utf-8 path")])
        .output()
        .expect("run kira")
}

/// A file of `count` trivial functions, which is `count * 2 + 2` lines.
fn filler(count: usize) -> String {
    let mut text = String::from("import Foundation\n\n@Main function main() {\n    return\n}\n");
    for index in 0..count {
        text.push_str(&format!(
            "\nfunction filler{index}() -> Int {{ return {index} }}\n"
        ));
    }
    text
}

const FILE_LENGTH_AT_40: &str = "import Foundation\n\n\
     Lint FileLength {\n\
     \x20   let code: String = \"KLINT003\"\n\
     \x20   let severity: String = \"warning\"\n\
     \x20   let enabled: Bool = true\n\
     \x20   let limit: Int = 40\n\
     }\n";

#[test]
fn reports_a_file_past_the_configured_ceiling() {
    let root = write_package(
        "lint_long",
        &[
            ("linter.kira", FILE_LENGTH_AT_40),
            ("app/main.kira", &filler(30)),
        ],
    );
    let output = kira_lint(&root);
    let text = String::from_utf8_lossy(&output.stdout).into_owned()
        + &String::from_utf8_lossy(&output.stderr);
    let _ = std::fs::remove_dir_all(&root);
    assert!(text.contains("KLINT003"), "{text}");
    assert!(text.contains("past the 40-line ceiling"), "{text}");
    // Once for the file, not once per declaration in it.
    assert_eq!(text.matches("KLINT003").count(), 1, "{text}");
    // On the last declaration, which is where the file grew past the ceiling.
    assert!(text.contains("function filler29"), "{text}");
}

#[test]
fn says_nothing_about_a_file_inside_the_ceiling() {
    let root = write_package(
        "lint_short",
        &[
            ("linter.kira", FILE_LENGTH_AT_40),
            ("app/main.kira", &filler(2)),
        ],
    );
    let output = kira_lint(&root);
    let text = String::from_utf8_lossy(&output.stdout).into_owned()
        + &String::from_utf8_lossy(&output.stderr);
    let _ = std::fs::remove_dir_all(&root);
    assert!(!text.contains("KLINT003"), "{text}");
}

#[test]
fn a_lint_left_disabled_reports_nothing() {
    let root = write_package(
        "lint_off",
        &[
            (
                "linter.kira",
                &FILE_LENGTH_AT_40.replace("enabled: Bool = true", "enabled: Bool = false"),
            ),
            ("app/main.kira", &filler(30)),
        ],
    );
    let output = kira_lint(&root);
    let text = String::from_utf8_lossy(&output.stdout).into_owned()
        + &String::from_utf8_lossy(&output.stderr);
    let _ = std::fs::remove_dir_all(&root);
    assert!(!text.contains("KLINT003"), "{text}");
}

#[test]
fn generated_bindings_are_never_measured() {
    // A `bindings/` directory is machine-written: a file per foreign API, as
    // long as that API is. Measuring it says nothing about how the package is
    // organized, and the generator would put it straight back.
    let root = write_package(
        "lint_bindings",
        &[
            ("linter.kira", FILE_LENGTH_AT_40),
            ("app/main.kira", &filler(2)),
            ("app/bindings/foreign.kira", &filler(30)),
        ],
    );
    let output = kira_lint(&root);
    let text = String::from_utf8_lossy(&output.stdout).into_owned()
        + &String::from_utf8_lossy(&output.stderr);
    let _ = std::fs::remove_dir_all(&root);
    assert!(!text.contains("KLINT003"), "{text}");
}

/// A package with no `linter.kira` asks for nothing, and gets nothing.
///
/// And is told so. "Nothing found" and "nothing ran" are opposite facts, and a
/// run that reports the first when the second is true is worse than one that
/// reports nothing at all — it is the shape of a green build that checked no
/// code. These four cases pin each outcome to its own sentence.
#[test]
fn a_package_that_configures_no_lints_is_told_nothing_was_checked() {
    let root = write_package("lint_none", &[("app/main.kira", &filler(30))]);
    let output = kira_lint(&root);
    let text = String::from_utf8_lossy(&output.stdout).into_owned()
        + &String::from_utf8_lossy(&output.stderr);
    let _ = std::fs::remove_dir_all(&root);
    assert!(!text.contains("KLINT"), "{text}");
    assert!(text.contains("no lint is enabled"), "{text}");
    assert!(text.contains("nothing was checked"), "{text}");
}

#[test]
fn a_clean_run_says_how_many_lints_ran() {
    let root = write_package(
        "lint_clean",
        &[
            ("linter.kira", FILE_LENGTH_AT_40),
            ("app/main.kira", &filler(2)),
        ],
    );
    let output = kira_lint(&root);
    let text = String::from_utf8_lossy(&output.stdout).into_owned()
        + &String::from_utf8_lossy(&output.stderr);
    let _ = std::fs::remove_dir_all(&root);
    // The count is the whole point: silence with a number behind it is a clean
    // run, silence without one is an absent one.
    assert!(text.contains("1 lint(s) ran, nothing found"), "{text}");
}

#[test]
fn a_run_that_found_something_says_what_it_ran() {
    let root = write_package(
        "lint_counted",
        &[
            ("linter.kira", FILE_LENGTH_AT_40),
            ("app/main.kira", &filler(30)),
        ],
    );
    let output = kira_lint(&root);
    let text = String::from_utf8_lossy(&output.stdout).into_owned()
        + &String::from_utf8_lossy(&output.stderr);
    let _ = std::fs::remove_dir_all(&root);
    assert!(text.contains("report(s) from 1 lint(s)"), "{text}");
}

/// `kira check` runs no lint at all, even where one is configured.
///
/// A lint runs during macro expansion, which every verb performs, so without a
/// gate the whole pass would be paid for and reported by `check`, `run` and
/// `build` alike. The gate is an environment variable the lint verb sets on
/// itself before compiling, read once at the frontend edge and turned into a
/// salsa input — so this asserts the absence of every `KLINT` code, receipt
/// included.
#[test]
fn check_runs_no_lint_even_where_one_is_configured() {
    let root = write_package(
        "lint_check_quiet",
        &[
            ("linter.kira", FILE_LENGTH_AT_40),
            ("app/main.kira", &filler(30)),
        ],
    );
    let foundation = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../foundation")
        .canonicalize()
        .expect("the checkout's foundation");
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_kira"))
        .env("KIRA_FOUNDATION_HOME", foundation)
        // Explicitly cleared: a developer with it set in their shell must not
        // make this pass or fail for a reason the test is not about.
        .env_remove("KIRA_LINT")
        .args(["check", root.to_str().expect("a utf-8 path")])
        .output()
        .expect("run kira");
    let text = String::from_utf8_lossy(&output.stdout).into_owned()
        + &String::from_utf8_lossy(&output.stderr);
    let _ = std::fs::remove_dir_all(&root);
    assert!(!text.contains("KLINT"), "{text}");
}

/// The receipt is not a finding, so it is never printed as one.
#[test]
fn the_runners_receipt_is_consumed_rather_than_reported() {
    let root = write_package(
        "lint_receipt",
        &[
            ("linter.kira", FILE_LENGTH_AT_40),
            ("app/main.kira", &filler(2)),
        ],
    );
    let output = kira_lint(&root);
    let text = String::from_utf8_lossy(&output.stdout).into_owned()
        + &String::from_utf8_lossy(&output.stderr);
    let _ = std::fs::remove_dir_all(&root);
    assert!(!text.contains("KLINT000"), "{text}");
    assert!(!text.contains("lints ran:"), "{text}");
}
