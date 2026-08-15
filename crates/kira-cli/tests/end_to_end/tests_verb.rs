//! Direct coverage for Foundation isolation and harness-owned KIK tests.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

const TEST_VOCABULARY: &str = include_str!("../../../../tests-kik/harness/app/Test.kira");
const TEST_RUNNER: &str = include_str!("../../../../tests-kik/harness/app/TestRunner.kira");

fn package(source: &str, with_test_support: bool) -> PathBuf {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
    let directory =
        std::env::temp_dir().join(format!("kira_test_verb_{}_{}", std::process::id(), unique));
    let app = directory.join("app");
    std::fs::create_dir_all(&app).expect("test package directory");
    std::fs::write(
        directory.join("package.kira"),
        "Package KikTestRegression {\n    let version = \"0.1.0\"\n    let kind = .App\n    let moduleRoot = \"KikTestRegression\"\n}\n",
    )
    .expect("test package manifest");
    std::fs::write(app.join("main.kira"), source).expect("test package source");
    if with_test_support {
        std::fs::write(app.join("Test.kira"), TEST_VOCABULARY).expect("test vocabulary");
        std::fs::write(app.join("TestRunner.kira"), TEST_RUNNER).expect("test runner");
    }
    directory
}

fn kira(args: &[&str]) -> std::process::Output {
    let foundation = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../foundation")
        .canonicalize()
        .expect("the checkout's Foundation");
    std::process::Command::new(env!("CARGO_BIN_EXE_kira"))
        .env("KIRA_FOUNDATION_HOME", foundation)
        .args(args)
        .output()
        .expect("run kira")
}

fn remove_package(path: &Path) {
    let _ = std::fs::remove_dir_all(path);
}

#[test]
fn a_normal_foundation_app_checks_and_runs_without_test_runner_expansion() {
    let path = package(
        "import Foundation\n@Main function main() { printLine(\"ordinary\") return }\n",
        false,
    );
    let path_text = path.to_str().expect("a utf-8 package path");

    let checked = kira(&["check", path_text]);
    assert!(
        checked.status.success(),
        "ordinary app did not check: {}",
        String::from_utf8_lossy(&checked.stderr)
    );

    let run = kira(&["run", "--backend", "vm", path_text]);
    assert!(
        run.status.success(),
        "ordinary app did not run: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout), "ordinary\n");

    let tested = kira(&["test", path_text]);
    assert!(!tested.status.success());
    assert!(
        String::from_utf8_lossy(&tested.stderr).contains("no tests to run"),
        "Foundation supplied a test entrypoint: {}",
        String::from_utf8_lossy(&tested.stderr)
    );
    remove_package(&path);
}

#[test]
fn harness_owned_test_declarations_compile_and_run_in_test_mode() {
    let path = package(
        "import Foundation\n\
         Test SumsToTen {\n\
             test { return 4 + 6 }\n\
             expect { let e: Result<Int, TestFailure> = .Ok(10) return e }\n\
         }\n\
         Test DoesNot {\n\
             test { return 1 }\n\
             expect { let e: Result<Int, TestFailure> = .Ok(2) return e }\n\
         }\n",
        true,
    );
    let path_text = path.to_str().expect("a utf-8 package path");
    let output = kira(&["test", "--backend", "vm", path_text]);
    assert!(
        !output.status.success(),
        "harness tests did not run: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "ok   SumsToTen\nFAIL DoesNot\n1 passed, 1 failed, 0 skipped, 2 total\n"
    );
    remove_package(&path);
}

#[test]
fn harness_owned_dispatch_selects_one_test_in_test_mode() {
    let path = package(
        "import Foundation\n\
         Test SumsToTen {\n\
             test { return 4 + 6 }\n\
             expect { let e: Result<Int, TestFailure> = .Ok(10) return e }\n\
         }\n",
        true,
    );
    let path_text = path.to_str().expect("a utf-8 package path");
    let output = kira(&[
        "test",
        "--backend",
        "vm",
        path_text,
        "--",
        "check",
        "SumsToTen",
    ]);
    assert!(
        output.status.success(),
        "test dispatch did not run: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "__kira_test_result__:0\n"
    );
    remove_package(&path);
}

#[test]
fn a_harness_with_no_cases_reports_an_empty_run() {
    let path = package(
        "import Foundation\n@Main function main() { printLine(\"not a suite\") return }\n",
        true,
    );
    let path_text = path.to_str().expect("a utf-8 package path");
    let output = kira(&["test", path_text]);
    assert!(
        output.status.success(),
        "empty harness did not run: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "0 passed, 0 failed, 0 skipped, 0 total\n"
    );
    remove_package(&path);
}
