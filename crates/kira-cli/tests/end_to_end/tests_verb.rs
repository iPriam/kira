//! `kira test`, driven through the real binary.
//!
//! What these prove is that the whole chain holds through the *shipped*
//! compiler and the *shipped* Foundation: a `Test` declaration is found by a
//! collector macro written in Kira, the runner it generates is compiled like
//! any other function, and `kira test` enters it instead of `@Main`. Nothing
//! in the compiler names `Test`, so if the chain broke anywhere the verb would
//! report a program with no tests rather than quietly passing.

use crate::write_source;

/// Runs `kira test` against the Foundation **in this checkout**.
///
/// The other end-to-end modules deliberately exercise the *installed*
/// Foundation, which is the right target for a discovery contract. These cases
/// are about the runner Foundation ships, so they pin the source tree's copy:
/// otherwise they would test whichever toolchain was last installed and fail
/// or pass for a reason that has nothing to do with this checkout.
fn kira_test(path: &std::path::Path) -> std::process::Output {
    let foundation = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../foundation")
        .canonicalize()
        .expect("the checkout's foundation");
    std::process::Command::new(env!("CARGO_BIN_EXE_kira"))
        .env("KIRA_FOUNDATION_HOME", foundation)
        .args(["test", path.to_str().expect("a utf-8 path")])
        .output()
        .expect("run kira")
}

/// A suite with no `@Main` at all, which is the case the verb exists for.
///
/// `kira run` would refuse this program — an application needs an entrypoint —
/// so a suite compiling and running here is the whole feature.
#[test]
fn runs_a_suite_that_has_no_main() {
    let path = write_source(
        "import Foundation\n\
         Test SumsToTen {\n\
             test { return 4 + 6 }\n\
             expect { let e: Result<Int, TestFailure> = .Ok(10) return e }\n\
         }",
    );
    let output = kira_test(&path);
    let _ = std::fs::remove_file(&path);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "ok   SumsToTen\n1 passed, 0 failed, 0 skipped, 1 total\n"
    );
}

/// A case whose answer differs from its expectation is reported as a failure
/// rather than passing quietly, and the run still reaches the ones after it.
#[test]
fn reports_a_case_that_does_not_hold() {
    let path = write_source(
        "import Foundation\n\
         Test Holds {\n\
             test { return 1 }\n\
             expect { let e: Result<Int, TestFailure> = .Ok(1) return e }\n\
         }\n\
         Test DoesNot {\n\
             test { return 1 }\n\
             expect { let e: Result<Int, TestFailure> = .Ok(2) return e }\n\
         }",
    );
    let output = kira_test(&path);
    let _ = std::fs::remove_file(&path);
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "ok   Holds\nFAIL DoesNot\n1 passed, 1 failed, 0 skipped, 2 total\n"
    );
}

/// A case may answer with whatever it measures, because a `Test` member returns
/// `Any` and the comparison behind it is structural.
///
/// The `String` case is the one that would pass for the wrong reason if `Any`
/// equality compared handles rather than contents: the two strings are built
/// differently and are never one object.
#[test]
fn a_case_may_answer_with_any_type_it_measures() {
    let path = write_source(
        "import Foundation\n\
         struct Point { var x: Int = 0\n\
             var y: Int = 0 }\n\
         Test Text {\n\
             test { return \"he\" + \"llo\" }\n\
             expect { let e: Result<String, TestFailure> = .Ok(\"hello\") return e }\n\
         }\n\
         Test Truth {\n\
             test { return 1 < 2 }\n\
             expect { let e: Result<Bool, TestFailure> = .Ok(true) return e }\n\
         }\n\
         Test Shape {\n\
             test { return Point(x: 1, y: 2) }\n\
             expect { let e: Result<Point, TestFailure> = .Ok(Point(x: 1, y: 2)) return e }\n\
         }",
    );
    let output = kira_test(&path);
    let _ = std::fs::remove_file(&path);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "ok   Text\nok   Truth\nok   Shape\n3 passed, 0 failed, 0 skipped, 3 total\n"
    );
}

/// A program that imports Foundation but declares no case runs an empty suite.
///
/// The collector emits a runner whether or not it found anything, so this
/// reports an empty run rather than an error: "no tests here" is an answer, and
/// a suite that has had its last case deleted should say so rather than start
/// failing the build.
#[test]
fn a_suite_with_no_cases_runs_empty() {
    let path = write_source(
        "import Foundation\n\
         @Main function main() { printLine(\"not a suite\") return }",
    );
    let output = kira_test(&path);
    let _ = std::fs::remove_file(&path);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "0 passed, 0 failed, 0 skipped, 0 total\n"
    );
}

/// A program that never imports Foundation has no runner at all, and is told so
/// by name rather than refused as a library with no entrypoint.
///
/// The collector lives in Foundation, so a program that does not import it runs
/// no collector and generates nothing — which is the honest reason there is
/// nothing to enter.
#[test]
fn a_program_without_foundation_says_it_has_no_tests() {
    let path = write_source("@Main function main() { print(\"plain\") return }");
    let output = kira_test(&path);
    let _ = std::fs::remove_file(&path);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("no tests to run"), "{stderr}");
}
