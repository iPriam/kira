//! Reading a test run: the tally, the failures, and the shorthands that pick
//! what runs. Split from `suite.rs` on the file-size ladder.

use super::*;
use crate::session;

/// Per-binary tallies sum; reading only the last would report one binary.
#[test]
fn the_tally_sums_every_binary() {
    let stdout = "test result: ok. 10 passed; 0 failed; 1 ignored; 0 measured\n\
                  test result: FAILED. 3 passed; 2 failed; 0 ignored; 0 measured\n";
    assert_eq!(
        parse_tally(stdout),
        Tally {
            passed: 13,
            failed: 2,
            skipped: 1
        }
    );
}

#[test]
fn each_failing_test_becomes_one_structured_failure() {
    let run = Run {
        command: vec!["cargo".to_owned()],
        cwd: ".".to_owned(),
        exit_code: Some(101),
        timed_out: false,
        duration_seconds: 1.0,
        stdout: "test a::b ... FAILED\ntest c::d ... FAILED\n\
                 ---- a::b stdout ----\n\nassertion failed: left != right\n"
            .to_owned(),
        stderr: String::new(),
    };
    let failures = parse_failures(&run);
    assert_eq!(failures.len(), 2);
    assert_eq!(failures[0].kind, FailureKind::TestFailure);
    assert!(
        failures[0]
            .backtrace
            .as_deref()
            .is_some_and(|text| text.contains("assertion failed")),
        "the panic block belongs to the test that printed it"
    );
}

/// A failing run that named no test still reports a failure.
#[test]
fn a_run_that_fails_without_naming_a_test_is_not_a_pass() {
    let run = Run {
        command: vec!["cargo".to_owned()],
        cwd: ".".to_owned(),
        exit_code: Some(101),
        timed_out: false,
        duration_seconds: 1.0,
        stdout: String::new(),
        stderr: "error[E0308]: mismatched types".to_owned(),
    };
    let failures = parse_failures(&run);
    assert_eq!(failures.len(), 1);
    assert_eq!(failures[0].kind, FailureKind::Crash);
}

#[test]
fn a_timed_out_run_is_reported_as_a_timeout() {
    let run = Run {
        command: vec!["cargo".to_owned()],
        cwd: ".".to_owned(),
        exit_code: None,
        timed_out: true,
        duration_seconds: 1.0,
        stdout: String::new(),
        stderr: String::new(),
    };
    assert_eq!(parse_failures(&run)[0].kind, FailureKind::Timeout);
}

/// A file is the shorthand: the crate that owns it, filtered to its own name.
#[test]
fn a_file_names_both_the_package_and_the_filter() {
    assert_eq!(
        package_of("crates/kira-runtime-abi/src/bridge.rs").as_deref(),
        Some("kira-runtime-abi")
    );
    assert_eq!(
        package_of("crates\\kira-ir\\src\\mid.rs").as_deref(),
        Some("kira-ir")
    );
    assert_eq!(
        file_stem("crates/kira-runtime-abi/src/bridge.rs").as_deref(),
        Some("bridge")
    );
}

/// A path outside `crates/` names no package, so it narrows nothing rather
/// than narrowing to something invented.
#[test]
fn a_path_outside_a_crate_names_no_package() {
    assert_eq!(package_of("AGENTS.md"), None);
}

/// A suite whose tests live in their own binary selects that binary.
///
/// `cargo test -p kira-cli -- backend_parity` matched nothing and exited zero:
/// a binary's name is no part of any test's name. The gate ran the parity
/// suite for months and proved nothing by it.
#[test]
fn a_suite_that_names_a_test_binary_selects_it_with_the_target_flag() {
    let args = cargo_arguments(
        Some("backend_parity"),
        None,
        "debug",
        None,
        Fallback::Workspace,
    );
    assert_eq!(
        args,
        vec![
            "test",
            "-p",
            "kira-cli",
            "--test",
            "backend_parity",
            "--no-fail-fast"
        ]
    );
}

/// A suite whose tests are a module of a shared binary still narrows by name.
#[test]
fn a_suite_that_names_a_module_still_filters_by_name() {
    let args = cargo_arguments(Some("hybrid"), None, "debug", None, Fallback::Workspace);
    assert_eq!(
        args,
        vec![
            "test",
            "-p",
            "kira-hybrid-runtime",
            "-p",
            "kira-cli",
            "--no-fail-fast",
            "--",
            "seam"
        ]
    );
}

/// The toolchain suite is the one selection that runs the `#[ignore]`d
/// self-install proofs — each builds and installs this checkout — so it must
/// pass `--include-ignored` behind the `--` separator, and no other suite may.
#[test]
fn the_toolchain_suite_alone_runs_the_ignored_install_proofs() {
    let args = cargo_arguments(Some("toolchain"), None, "debug", None, Fallback::Workspace);
    assert_eq!(
        args,
        vec![
            "test",
            "-p",
            "kira-cli",
            "-p",
            "kira-build",
            "-p",
            "kira-project",
            "-p",
            "kira-knvm",
            "--no-fail-fast",
            "--",
            "--include-ignored"
        ]
    );
    let all = cargo_arguments(Some("all"), None, "debug", None, Fallback::Workspace);
    assert!(
        !all.iter().any(|argument| argument == "--include-ignored"),
        "the default gate must leave the install proofs ignored"
    );
}

/// A caller's own package wins over the suite's, and the suite's binary is not
/// forced onto it: a package with no such target would fail the run outright.
#[test]
fn a_named_package_drops_the_suites_target() {
    let args = cargo_arguments(
        Some("backend_parity"),
        Some("kira-vm-runtime"),
        "debug",
        None,
        Fallback::Workspace,
    );
    assert_eq!(
        args,
        vec!["test", "-p", "kira-vm-runtime", "--no-fail-fast"]
    );
}

/// A named suite that matched no test proved nothing, whatever cargo exited.
#[test]
fn a_suite_that_matched_no_test_is_not_a_pass() {
    let run = Run {
        command: vec!["cargo".to_owned(), "test".to_owned()],
        cwd: ".".to_owned(),
        exit_code: Some(0),
        timed_out: false,
        duration_seconds: 1.0,
        stdout: "test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured\n".to_owned(),
        stderr: String::new(),
    };
    let tally = parse_tally(&run.stdout);
    let failure = ran_nothing("backend_parity", &run, &tally).expect("an empty suite is reported");
    assert_eq!(failure.kind, FailureKind::CapabilityMissing);

    // A suite that ran tests is left alone.
    let ran = Run {
        stdout: "test result: ok. 12 passed; 0 failed; 0 ignored; 0 measured\n".to_owned(),
        ..run
    };
    assert!(ran_nothing("backend_parity", &ran, &parse_tally(&ran.stdout)).is_none());
}

/// An unknown suite is refused before anything is spawned, so a typo never
/// silently becomes the whole workspace.
#[test]
fn an_unknown_suite_is_refused_without_running_anything() {
    let refused = run(
        &json!({ "suite": "everything" }),
        Duration::from_secs(1),
        Fallback::Workspace,
    );
    assert!(refused.is_err());
}

/// A summary still lists what failed; only the evidence is left behind.
#[test]
fn a_summary_keeps_one_entry_per_failure_and_drops_the_output() {
    let whole = json!({
        "success": false,
        "failed": 1,
        "failures": [{
            "kind": "test_failure",
            "message": "`a::b` failed",
            "backtrace": "stack backtrace: ...",
            "stdout": "a great deal of output",
            "stderr": "",
            "command": ["cargo", "test"],
            "artifacts": [],
            "reproduction": null,
        }],
        "stdout": "a great deal of output",
        "stderr": "",
    });

    let summary = super::super::project("summary", whole.clone());
    assert_eq!(summary["failures"].as_array().expect("failures").len(), 1);
    assert_eq!(summary["failures"][0]["message"], json!("`a::b` failed"));
    assert_eq!(summary["failures"][0]["backtrace"], Value::Null);
    assert_eq!(summary["stdout"], json!(""));
    assert_eq!(summary["output_omitted"], json!(true));

    let failures = super::super::project("failures", whole.clone());
    assert_eq!(
        failures["failures"][0]["backtrace"],
        json!("stack backtrace: ...")
    );
    assert_eq!(failures["stdout"], json!(""));

    let full = super::super::project("full", whole);
    assert_eq!(full["stdout"], json!("a great deal of output"));
    assert_eq!(full["output_omitted"], Value::Null);
}

/// A summarized failure reads back in full through its saved run.
#[test]
fn a_saved_run_reads_back_the_detail_the_summary_left_out() {
    let whole = json!({
        "success": false,
        "failed": 1,
        "failures": [{
            "kind": "test_failure",
            "message": "`a::b` failed",
            "backtrace": "stack backtrace: core::panicking",
            "stdout": "assertion failed",
            "stderr": "",
        }],
        "stdout": "assertion failed",
        "stderr": "",
    });
    let id = session::store("validate", &whole).expect("the run is saved");

    let summary = super::super::project("summary", whole);
    assert_eq!(summary["failures"][0]["backtrace"], Value::Null);

    let (replayed, is_error) =
        super::super::investigate::debug(&json!({ "session": id, "detail": "full" }));
    assert!(is_error, "replaying a failure does not make it a pass");
    assert_eq!(replayed["replayed"], json!(true));
    assert_eq!(
        replayed["failures"][0]["backtrace"],
        json!("stack backtrace: core::panicking")
    );
    assert_eq!(replayed["stdout"], json!("assertion failed"));
}

/// Narrowing keeps only the named failure.
#[test]
fn a_saved_run_can_be_narrowed_to_one_failure() {
    let whole = json!({
        "success": false,
        "failures": [
            { "kind": "test_failure", "message": "`a::b` failed", "stdout": "", "stderr": "" },
            { "kind": "test_failure", "message": "`c::d` failed", "stdout": "", "stderr": "" },
        ],
        "stdout": "",
        "stderr": "",
    });
    let id = session::store("validate", &whole).expect("the run is saved");
    let (replayed, _) = super::super::investigate::debug(&json!({ "session": id, "test": "c::d" }));
    assert_eq!(replayed["narrowed_to"], json!("c::d"));
    let failures = replayed["failures"].as_array().expect("failures");
    assert_eq!(failures.len(), 1);
    assert_eq!(failures[0]["message"], json!("`c::d` failed"));
}

/// An identifier nothing was saved under is an error, not an empty pass.
#[test]
fn an_unknown_session_is_not_reported_as_a_run_with_no_failures() {
    let (result, is_error) =
        super::super::investigate::debug(&json!({ "session": "test-0000000000000-9999" }));
    assert!(is_error);
    assert_eq!(result["replayed"], json!(false));
    assert_eq!(result["success"], json!(false));
}
