//! `kira_dev_validate`: the repository's development gate.
//!
//! One tool, not two. A gate that could be passed without its tests was a gate
//! with a way around it, and "run the other tool as well" is a rule an agent
//! can forget. So the tests are a phase of this call: `test` decides whether
//! they run, and the fields in [`suite::schema_properties`] decide which.

use std::path::Path;

use serde_json::{Value, json};

use super::{DETAILS, bool_field, enum_field, string_list, suite, timeout};
use crate::exec::{self, Run};
use crate::schema::{Diagnostic, Failure, FailureKind};
use crate::session;

/// This server's own crate, which the gate compiles but cannot relink.
const SELF_CRATE: &str = "kira-mcp";

/// One check the gate runs.
struct Check {
    name: &'static str,
    program: &'static str,
    args: Vec<String>,
}

pub fn descriptor() -> Value {
    let mut properties = json!({
        "scope": { "type": "string", "enum": ["workspace", "changed"] },
        "changed_files": { "type": "array", "items": { "type": "string" } },
        "full": { "type": "boolean" },
        "fix": { "type": "boolean" },
        "test": {
            "type": "boolean",
            "description": "Whether to run tests as part of the gate. True by default, because a gate reported green without them is not one. Set false only to iterate on formatting, lints and compilation."
        },
        "target": { "type": "string" },
        "environment": { "type": "object", "additionalProperties": { "type": "string" } },
        "detail": { "type": "string", "enum": DETAILS },
        "timeout": { "type": "integer", "minimum": 1 }
    });
    if let (Some(properties), Some(selection)) = (
        properties.as_object_mut(),
        suite::schema_properties().as_object(),
    ) {
        for (field, schema) in selection {
            properties.insert(field.clone(), schema.clone());
        }
    }
    json!({
        "name": "kira_dev_validate",
        "description": "Run the repository development gate: formatting, lints, compilation, architectural and file-size rules, and Kira's own tests.",
        "inputSchema": { "type": "object", "properties": properties }
    })
}

pub fn call(arguments: &Value) -> (Value, bool) {
    let changed = match string_list(arguments, "changed_files") {
        Ok(changed) => changed,
        Err(rejection) => return rejection,
    };
    let full = match bool_field(arguments, "full", changed.is_empty()) {
        Ok(full) => full,
        Err(rejection) => return rejection,
    };
    let fix = match bool_field(arguments, "fix", false) {
        Ok(fix) => fix,
        Err(rejection) => return rejection,
    };
    // Tests run unless the caller says otherwise. The default is the one an
    // agent gets by forgetting the flag, so it is the whole bar rather than
    // the fast half of it.
    let with_tests = match bool_field(arguments, "test", true) {
        Ok(with_tests) => with_tests,
        Err(rejection) => return rejection,
    };
    if let Err(rejection) = refuse_stray_selection(arguments, with_tests) {
        return rejection;
    }
    // Every input is read before anything is spawned. The test phase runs
    // last, and a typo in `suite` found there would be found after the minutes
    // the earlier checks cost.
    let detail = match enum_field(arguments, "detail", &DETAILS, Some("summary")) {
        Ok(detail) => detail.unwrap_or("summary"),
        Err(rejection) => return rejection,
    };
    if let Err(rejection) = enum_field(arguments, "suite", &suite::SUITES, None) {
        return rejection;
    }
    let bound = match timeout(arguments) {
        Ok(bound) => bound,
        Err(rejection) => return rejection,
    };

    let root = exec::repository_root();
    let mut checks: Vec<Value> = Vec::new();
    let mut violations: Vec<Failure> = Vec::new();
    let mut diagnostics: Vec<Diagnostic> = Vec::new();
    let mut runs: Vec<Run> = Vec::new();
    let mut fixes: Vec<String> = Vec::new();

    let planned = planned_checks(fix);
    if fix {
        fixes.push("cargo fmt --all".to_owned());
    }

    // One budget for the call, spent down by each check in turn. Handing every
    // check the full timeout would make the bound a caller asked for the bound
    // on one command, and the call could take that many times longer.
    let deadline = exec::Deadline::new(bound);
    for check in planned {
        if deadline.expired() {
            violations.push(Failure::new(
                FailureKind::Timeout,
                format!("`{}` did not run: the gate's time ran out", check.name),
            ));
            checks.push(json!({ "name": check.name, "passed": false, "ran": false }));
            continue;
        }
        let run = match exec::run(check.program, &check.args, &root, &[], deadline.remaining()) {
            Ok(run) => run,
            Err(error) => {
                violations.push(Failure::new(FailureKind::Crash, error.to_string()));
                checks.push(json!({ "name": check.name, "passed": false }));
                continue;
            }
        };
        let passed = run.success();
        if !passed {
            let kind = match run.timed_out {
                true => FailureKind::Timeout,
                false => FailureKind::Diagnostic,
            };
            violations
                .push(Failure::new(kind, format!("`{}` did not pass", check.name)).with_run(&run));
            diagnostics.push(Diagnostic::message(
                "error",
                format!("`{}` failed", check.name),
            ));
        }
        checks.push(json!({
            "name": check.name,
            "passed": passed,
            "duration_seconds": run.duration_seconds,
        }));
        runs.push(run);
    }

    // The tests, last, because a failing build makes their result meaningless
    // and their minutes wasted.
    //
    // A changed-files list narrows *which* tests run, never whether they run at
    // all: a gate that skipped the parity suite because it looked costly would
    // pass exactly the changes that need it most.
    let mut tally = None;
    if with_tests {
        let fallback = match (full, affected_packages(&changed)) {
            (false, affected) if !affected.is_empty() => suite::Fallback::Packages(affected),
            _ => suite::Fallback::Workspace,
        };
        match suite::run(arguments, deadline.remaining(), fallback) {
            Err(rejection) => return rejection,
            Ok(outcome) => {
                let passed = outcome.success();
                if !passed {
                    violations.extend(outcome.failures);
                    diagnostics.extend(outcome.diagnostics);
                }
                checks.push(json!({
                    "name": outcome.name,
                    "passed": passed,
                    "duration_seconds": outcome.run.duration_seconds,
                }));
                tally = Some(outcome.tally);
                runs.push(outcome.run);
            }
        }
    } else {
        // Said out loud. A caller reading `checks` must not have to notice that
        // the entry they expected is absent.
        checks.push(json!({ "name": "tests", "passed": null, "skipped": true }));
        diagnostics.push(Diagnostic::message(
            "warning",
            "tests did not run (`test` was false); this run is not the gate",
        ));
    }

    // The rules cargo does not check: the file-size ladder and the skill
    // ceiling, both stated in AGENTS.md.
    let (size_checks, size_violations) = size_rules(&root);
    checks.extend(size_checks);
    violations.extend(size_violations);

    let success = violations.is_empty();
    let last = runs.last();
    // Every check's output, saved whole. The gate's own answer is a list of
    // pass and fail; the compiler output that explains a fail is what a caller
    // needs next, and re-running the gate to get it costs minutes.
    let whole = json!({
        "success": success,
        "tests_ran": with_tests,
        "passed": tally.as_ref().map(|tally| tally.passed),
        "failed": tally.as_ref().map(|tally| tally.failed),
        "skipped": tally.as_ref().map(|tally| tally.skipped),
        "failing_tests": violations
            .iter()
            .filter(|failure| failure.kind == FailureKind::TestFailure)
            .map(|failure| failure.message.clone())
            .collect::<Vec<_>>(),
        "checks": checks,
        "violations": violations,
        "diagnostics": diagnostics,
        "fixes_applied": fixes,
        "commands": runs.iter().map(exec::run_json).collect::<Vec<_>>(),
        "failures": runs
            .iter()
            .filter(|run| !run.success())
            .map(|run| Failure::new(FailureKind::Diagnostic, "a check did not pass").with_run(run))
            .collect::<Vec<_>>(),
        "stdout": last.map(|run| run.stdout.clone()).unwrap_or_default(),
        "stderr": last.map(|run| run.stderr.clone()).unwrap_or_default(),
    });
    let session = session::store("validate", &whole).ok();

    let mut result = super::project(detail, whole);
    result["session"] = json!(session);
    (result, !success)
}

/// The commands the gate runs before its tests, in order.
fn planned_checks(fix: bool) -> Vec<Check> {
    vec![
        Check {
            name: "formatting",
            program: "cargo",
            args: match fix {
                true => exec::argv(&["fmt", "--all"]),
                false => exec::argv(&["fmt", "--check"]),
            },
        },
        Check {
            name: "lints",
            program: "cargo",
            args: exec::argv(&[
                "clippy",
                "--workspace",
                "--all-targets",
                "--",
                "-D",
                "warnings",
            ]),
        },
        // Everything but this server, which is running: Windows will not let
        // cargo replace an executable that is executing, and this gate is
        // served from `target/debug/kira-mcp.exe`. Excluding it is not a gap,
        // because `self_compilation` below compiles it — `cargo check` never
        // links, so it never needs to replace the file. Splitting the two is
        // what keeps the gate honest about its own crate rather than reporting
        // a failure that means only "the server is up".
        Check {
            name: "workspace_compilation",
            program: "cargo",
            args: exec::argv(&["build", "--workspace", "--exclude", SELF_CRATE]),
        },
        Check {
            name: "self_compilation",
            program: "cargo",
            args: exec::argv(&["check", "-p", SELF_CRATE, "--all-targets"]),
        },
        Check {
            name: "portable_core",
            program: "cargo",
            args: exec::argv(&[
                "check",
                "-p",
                "kira-vm-runtime",
                "--target",
                "wasm32-unknown-unknown",
            ]),
        },
    ]
}

/// Refuses a test selection alongside `test: false`.
///
/// The two say opposite things, and honouring either silently is worse than
/// refusing: running the suite ignores the flag, skipping it ignores the
/// selection and answers "gate passed" about tests that never ran.
fn refuse_stray_selection(arguments: &Value, with_tests: bool) -> Result<(), (Value, bool)> {
    if with_tests {
        return Ok(());
    }
    for field in [
        "suite",
        "package",
        "crate",
        "file",
        "filter",
        "update_snapshots",
    ] {
        if !matches!(arguments.get(field), None | Some(Value::Null)) {
            return Err(super::invalid(
                field,
                "names tests to run, but `test` is false",
            ));
        }
    }
    Ok(())
}

/// The crates a changed-file list touches.
fn affected_packages(changed: &[String]) -> Vec<String> {
    let mut packages: Vec<String> = Vec::new();
    for file in changed {
        let normalized = file.replace('\\', "/");
        let Some(rest) = normalized.split("crates/").nth(1) else {
            continue;
        };
        let Some(package) = rest.split('/').next() else {
            continue;
        };
        if !package.is_empty() && !packages.iter().any(|known| known == package) {
            packages.push(package.to_owned());
        }
    }
    packages
}

/// The file-size rules, which no lint enforces.
fn size_rules(root: &Path) -> (Vec<Value>, Vec<Failure>) {
    let mut violations = Vec::new();
    let mut over_skill = Vec::new();

    let skills = root.join(".codex/skills");
    if let Ok(entries) = std::fs::read_dir(&skills) {
        for entry in entries.flatten() {
            let file = entry.path().join("SKILL.md");
            let Ok(text) = std::fs::read_to_string(&file) else {
                continue;
            };
            let lines = text.lines().count();
            if lines > 80 {
                over_skill.push(format!("{} is {lines} lines", file.display()));
            }
        }
    }
    for detail in &over_skill {
        violations.push(Failure::new(
            FailureKind::InvalidArtifact,
            format!("skill over the 80-line ceiling: {detail}"),
        ));
    }

    (
        vec![json!({
            "name": "file_size_rules",
            "passed": over_skill.is_empty(),
            "detail": over_skill,
        })],
        violations,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn changed_files_map_to_the_crates_that_own_them() {
        let changed = vec![
            "crates/kira-ir/src/mid.rs".to_owned(),
            "crates\\kira-ir\\src\\lower.rs".to_owned(),
            "crates/kira-cli/tests/backend_parity/seam.rs".to_owned(),
            "AGENTS.md".to_owned(),
        ];
        let packages = affected_packages(&changed);
        assert_eq!(packages, vec!["kira-ir", "kira-cli"]);
    }

    /// A file outside `crates/` narrows nothing, so the gate stays whole.
    #[test]
    fn a_change_outside_a_crate_leaves_the_scope_full() {
        assert!(affected_packages(&["AGENTS.md".to_owned()]).is_empty());
    }

    /// The merge, as the schema states it: one call carries both the flag and
    /// the fields that choose which tests run.
    #[test]
    fn the_gate_advertises_the_test_flag_and_the_suite_selection() {
        let properties = descriptor()["inputSchema"]["properties"].clone();
        for field in ["test", "suite", "package", "file", "filter", "fix"] {
            assert!(
                !properties[field].is_null(),
                "`{field}` must be on the gate's schema"
            );
        }
        assert_eq!(properties["test"]["type"], json!("boolean"));
    }

    /// The workspace build skips this crate, so something else has to compile
    /// it. A gate that excluded its own server and left it at that would stop
    /// noticing a break in the tool doing the checking.
    #[test]
    fn the_crate_excluded_from_the_build_is_compiled_by_its_own_check() {
        let planned = planned_checks(false);
        let build = planned
            .iter()
            .find(|check| check.name == "workspace_compilation")
            .expect("the workspace build");
        assert!(
            build
                .args
                .windows(2)
                .any(|pair| pair == ["--exclude", SELF_CRATE]),
            "the running server cannot be relinked, so the build must exclude it"
        );
        let own = planned
            .iter()
            .find(|check| check.name == "self_compilation")
            .expect("the server's own check");
        assert!(own.args.contains(&"check".to_owned()), "check never links");
        assert!(own.args.windows(2).any(|pair| pair == ["-p", SELF_CRATE]));
    }

    /// Omitting the flag runs the tests. The default is what an agent gets by
    /// forgetting, so forgetting must not be the way past the gate.
    #[test]
    fn tests_run_when_the_flag_is_omitted() {
        assert!(bool_field(&json!({}), "test", true).expect("a default"));
        assert!(!bool_field(&json!({ "test": false }), "test", true).expect("the flag"));
    }

    /// Naming tests while switching them off is refused rather than resolved:
    /// either reading of it answers about work that did not happen.
    #[test]
    fn a_selection_alongside_a_disabled_test_phase_is_refused() {
        for field in ["suite", "package", "crate", "file", "filter"] {
            let arguments = json!({ "test": false, field: "anything" });
            assert!(
                refuse_stray_selection(&arguments, false).is_err(),
                "`{field}` beside `test: false` must be refused"
            );
        }
        assert!(refuse_stray_selection(&json!({ "suite": "vm" }), true).is_ok());
        assert!(refuse_stray_selection(&json!({ "test": false }), false).is_ok());
    }

    /// The ceiling is checked against the real skills in this checkout.
    #[test]
    fn the_skill_ceiling_is_checked_against_this_repository() {
        let (checks, violations) = size_rules(&exec::repository_root());
        assert_eq!(checks[0]["name"], json!("file_size_rules"));
        assert!(
            violations.is_empty(),
            "a skill is over the ceiling: {violations:?}"
        );
    }
}
