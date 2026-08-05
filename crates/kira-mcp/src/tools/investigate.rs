//! `kira_dev_debug` and `kira_dev_reproduce`: what failed, and whether it
//! fails every time.
//!
//! Both run the failing thing rather than reasoning about it. `debug` runs it
//! once with the diagnostics turned up; `reproduce` runs it repeatedly, because
//! "it failed" and "it fails every time" are different facts and only the
//! second one makes a fix checkable.

use std::time::Duration;

use serde_json::{Value, json};

use super::program::{self, Configuration};
use super::{
    BACKENDS, DETAILS, DEVICES, bool_field, enum_field, environment, string_field, string_list,
    timeout, uint_field,
};
use crate::exec::{self, Run};
use crate::schema::{Diagnostic, Failure, FailureKind};
use crate::session;

/// The most a caller may ask for in one reproduce call.
///
/// A bound, so a mistyped `attempts` cannot turn one call into an afternoon.
const MAX_ATTEMPTS: u64 = 50;

pub fn debug_descriptor() -> Value {
    json!({
        "name": "kira_dev_debug",
        "description": "Inspect a failure: either read back a saved run by its `session`, or run a failing Kira program or test with diagnostics raised.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "session": {
                    "type": "string",
                    "description": "The `session` a previous kira_dev_test or kira_dev_validate returned. Reads that run's saved failures back and runs nothing."
                },
                "detail": { "type": "string", "enum": DETAILS },
                "source": { "type": "string" },
                "file": { "type": "string" },
                "test": {
                    "type": "string",
                    "description": "A test to run, or, beside `session`, the failure to narrow the saved run to."
                },
                "package": { "type": "string" },
                "arguments": { "type": "array", "items": { "type": "string" } },
                "backend": { "type": "string", "enum": BACKENDS },
                "device": { "type": "string", "enum": DEVICES },
                "check_leaks": { "type": "boolean" },
                "environment": { "type": "object", "additionalProperties": { "type": "string" } },
                "timeout": { "type": "integer", "minimum": 1 }
            }
        }
    })
}

pub fn reproduce_descriptor() -> Value {
    json!({
        "name": "kira_dev_reproduce",
        "description": "Re-run a Kira program or test several times to establish whether a failure is deterministic or intermittent.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "source": { "type": "string" },
                "file": { "type": "string" },
                "test": { "type": "string" },
                "package": { "type": "string" },
                "arguments": { "type": "array", "items": { "type": "string" } },
                "backend": { "type": "string", "enum": BACKENDS },
                "device": { "type": "string", "enum": DEVICES },
                "attempts": { "type": "integer", "minimum": 1, "maximum": MAX_ATTEMPTS },
                "environment": { "type": "object", "additionalProperties": { "type": "string" } },
                "timeout": { "type": "integer", "minimum": 1 }
            }
        }
    })
}

/// What both tools were asked to run.
struct Subject {
    configuration: Configuration,
    /// The Kira source, when the subject is a program.
    source: Option<String>,
    /// The test filter and package, when the subject is a test.
    test: Option<(String, Option<String>)>,
    program_args: Vec<String>,
    env: Vec<(String, String)>,
    timeout: Duration,
}

impl Subject {
    /// Runs the subject once.
    fn run_once(&self) -> Result<Run, exec::ExecError> {
        match (&self.source, &self.test) {
            (Some(source), _) => program::run(
                &self.configuration,
                source,
                &self.program_args,
                &self.env,
                self.timeout,
            ),
            (None, Some((filter, package))) => {
                let mut args = vec!["test".to_owned()];
                match package {
                    Some(package) => {
                        args.push("-p".to_owned());
                        args.push(package.clone());
                    }
                    None => args.push("--workspace".to_owned()),
                }
                args.push("--no-fail-fast".to_owned());
                args.push("--".to_owned());
                args.push(filter.clone());
                args.push("--nocapture".to_owned());
                exec::run(
                    "cargo",
                    &args,
                    &exec::repository_root(),
                    &self.env,
                    self.timeout,
                )
            }
            (None, None) => unreachable!("a subject always names a program or a test"),
        }
    }

    fn json(&self) -> Value {
        json!({
            "configuration": self.configuration.json(),
            "source": self.source,
            "test": self.test.as_ref().map(|(filter, _)| filter.clone()),
        })
    }
}

/// Reads the fields both tools share.
fn subject(arguments: &Value, extra_env: Vec<(String, String)>) -> Result<Subject, (Value, bool)> {
    let backend = enum_field(arguments, "backend", &BACKENDS, Some("vm"))?.unwrap_or("vm");
    let device = enum_field(arguments, "device", &DEVICES, Some("host"))?.unwrap_or("host");
    let source = string_field(arguments, "source")?
        .or(string_field(arguments, "file")?)
        .map(str::to_owned);
    let test = string_field(arguments, "test")?.map(str::to_owned);
    let package = string_field(arguments, "package")?.map(str::to_owned);
    let program_args = string_list(arguments, "arguments")?;
    let mut env = environment(arguments)?;
    let bound = timeout(arguments)?;

    if source.is_none() && test.is_none() {
        return Err(super::invalid(
            "source",
            "name either a Kira source file or a test to run",
        ));
    }
    // Raised diagnostics are the point of these two tools: a backtrace that was
    // never asked for is the one detail missing from every report of a panic.
    env.push(("RUST_BACKTRACE".to_owned(), "full".to_owned()));
    env.extend(extra_env);

    Ok(Subject {
        configuration: Configuration::new(backend, device),
        source,
        test: test.map(|filter| (filter, package)),
        program_args,
        env,
        timeout: bound,
    })
}

pub fn debug(arguments: &Value) -> (Value, bool) {
    // A saved run is read back rather than reproduced. Re-running the suite to
    // see what a caller was already told failed is the cost this exists to
    // avoid, and a second run can disagree with the first.
    match string_field(arguments, "session") {
        Err(rejection) => return rejection,
        Ok(Some(session)) => return replay(arguments, session),
        Ok(None) => {}
    }
    let leaks = match bool_field(arguments, "check_leaks", false) {
        Ok(leaks) => leaks,
        Err(rejection) => return rejection,
    };
    // The native heap oracle. Enabled only on request: it ends the process with
    // a non-zero status when a live allocation survives, which is exactly what a
    // leak hunt wants and exactly what an unrelated debug session does not.
    let extra = match leaks {
        true => vec![(
            kira_native_bridge::accounting::HEAP_REPORT_VAR.to_owned(),
            "1".to_owned(),
        )],
        false => Vec::new(),
    };
    let subject = match subject(arguments, extra) {
        Ok(subject) => subject,
        Err(rejection) => return rejection,
    };

    let run = match subject.run_once() {
        Ok(run) => run,
        Err(error) => return spawn_failure(&error),
    };

    let succeeded = run.success();
    let failure = (!succeeded).then(|| observed_failure(&subject, &run));
    (
        json!({
            "success": succeeded,
            "reproduced": !succeeded,
            "subject": subject.json(),
            "leak_check": leaks,
            "failures": failure.clone().into_iter().collect::<Vec<_>>(),
            "diagnostics": diagnostics_for(&run, succeeded),
            "commands": [exec::run_json(&run)],
            "stdout": run.stdout,
            "stderr": run.stderr,
        }),
        !succeeded,
    )
}

pub fn reproduce(arguments: &Value) -> (Value, bool) {
    let attempts = match uint_field(arguments, "attempts", 3) {
        Ok(0) => return super::invalid("attempts", "must be at least one"),
        Ok(attempts) if attempts > MAX_ATTEMPTS => {
            return super::invalid("attempts", &format!("must be at most {MAX_ATTEMPTS}"));
        }
        Ok(attempts) => attempts,
        Err(rejection) => return rejection,
    };
    let subject = match subject(arguments, Vec::new()) {
        Ok(subject) => subject,
        Err(rejection) => return rejection,
    };

    let mut runs: Vec<Run> = Vec::new();
    let mut failures: Vec<Failure> = Vec::new();
    for _ in 0..attempts {
        match subject.run_once() {
            Ok(run) => {
                if !run.success() {
                    failures.push(observed_failure(&subject, &run));
                }
                runs.push(run);
            }
            Err(error) => return spawn_failure(&error),
        }
    }

    let failed = failures.len() as u64;
    let verdict = match failed {
        0 => "not_reproduced",
        failed if failed == attempts => "deterministic",
        _ => "intermittent",
    };
    // A failure that reproduced is what this tool was asked to find, so it is
    // reported as a successful investigation. Only "it never failed" leaves the
    // caller without the answer they came for.
    let answered = failed > 0;
    (
        json!({
            "success": answered,
            "verdict": verdict,
            "attempts": attempts,
            "failed_attempts": failed,
            "subject": subject.json(),
            "failures": failures,
            "diagnostics": [Diagnostic::message(
                match answered { true => "warning", false => "note" },
                match verdict {
                    "deterministic" => format!("failed all {attempts} attempts"),
                    "intermittent" => format!("failed {failed} of {attempts} attempts"),
                    _ => format!("passed all {attempts} attempts; the failure did not reproduce"),
                },
            )],
            "commands": runs.iter().map(exec::run_json).collect::<Vec<_>>(),
            "stdout": runs.last().map(|run| run.stdout.clone()).unwrap_or_default(),
            "stderr": runs.last().map(|run| run.stderr.clone()).unwrap_or_default(),
        }),
        !answered,
    )
}

/// Reads a saved run back, optionally narrowed to one failure.
fn replay(arguments: &Value, id: &str) -> (Value, bool) {
    let detail = match enum_field(arguments, "detail", &DETAILS, Some("failures")) {
        Ok(detail) => detail.unwrap_or("failures"),
        Err(rejection) => return rejection,
    };
    let filter = match string_field(arguments, "test") {
        Ok(filter) => filter,
        Err(rejection) => return rejection,
    };
    let mut saved = match session::load(id) {
        Ok(saved) => saved,
        Err(error) => {
            // The run was not found, so there is nothing to report about it.
            // Answering with an empty failure list would read as "that run had
            // no failures", which is a claim about a run this server never saw.
            let failure = Failure::new(FailureKind::InvalidArtifact, error.to_string());
            return (
                json!({
                    "success": false,
                    "session": id,
                    "replayed": false,
                    "failures": [failure],
                    "diagnostics": [Diagnostic::message("error", error.to_string())],
                    "stdout": "",
                    "stderr": "",
                }),
                true,
            );
        }
    };

    let mut narrowed_to = Value::Null;
    if let (Some(filter), Some(failures)) = (filter, saved["failures"].as_array()) {
        let matching: Vec<Value> = failures
            .iter()
            .filter(|failure| {
                failure["message"]
                    .as_str()
                    .is_some_and(|message| message.contains(filter))
            })
            .cloned()
            .collect();
        narrowed_to = json!(filter);
        saved["failures"] = json!(matching);
    }

    let saved_success = saved["success"] == json!(true);
    let mut result = super::project(detail, saved);
    result["session"] = json!(id);
    result["replayed"] = json!(true);
    result["narrowed_to"] = narrowed_to;
    // The verdict belongs to the saved run: replaying a failure successfully
    // does not make the failure go away.
    (result, !saved_success)
}

/// The failure one bad run represents, classified.
fn observed_failure(subject: &Subject, run: &Run) -> Failure {
    let mut failure = Failure::new(classify(run), summary(run)).with_run(run);
    failure.backend = Some(subject.configuration.backend.clone());
    failure.target = Some(subject.configuration.device.clone());
    failure.backtrace = backtrace_of(run);
    failure.reproduction = Some(json!({
        "command": run.command,
        "cwd": run.cwd,
        "configuration": subject.configuration.json(),
    }));
    failure
}

/// Which kind of failure a run's output shows.
///
/// Classified from the output rather than the exit code alone: on every
/// platform this runs on, a panic and a segfault can both arrive as a non-zero
/// status, and telling them apart is the first thing a caller wants.
fn classify(run: &Run) -> FailureKind {
    let output = format!("{}{}", run.stdout, run.stderr);
    match () {
        () if run.timed_out => FailureKind::Timeout,
        () if output.contains("panicked at") => FailureKind::Panic,
        () if output.contains("error[") || output.contains("error: ") => FailureKind::Diagnostic,
        () if run.exit_code.is_none() => FailureKind::Crash,
        () => FailureKind::Crash,
    }
}

/// A one-line description of what went wrong.
fn summary(run: &Run) -> String {
    match classify(run) {
        FailureKind::Timeout => "the run did not terminate within the timeout".to_owned(),
        FailureKind::Panic => panic_line(run).unwrap_or_else(|| "the run panicked".to_owned()),
        FailureKind::Diagnostic => "the run failed to compile".to_owned(),
        _ => format!("the run exited {:?}", run.exit_code),
    }
}

/// The `panicked at` line, when there is one.
fn panic_line(run: &Run) -> Option<String> {
    format!("{}{}", run.stdout, run.stderr)
        .lines()
        .find(|line| line.contains("panicked at"))
        .map(|line| line.trim().to_owned())
}

/// The backtrace a run printed, when it printed one.
fn backtrace_of(run: &Run) -> Option<String> {
    let combined = format!("{}\n{}", run.stdout, run.stderr);
    let start = combined.find("stack backtrace:")?;
    Some(combined[start..].trim().to_owned())
}

/// A note when a run that was expected to fail did not.
fn diagnostics_for(run: &Run, succeeded: bool) -> Vec<Diagnostic> {
    match succeeded {
        true => vec![Diagnostic::message(
            "note",
            "the subject succeeded, so there was no failure to inspect",
        )],
        false => vec![Diagnostic::message("error", summary(run))],
    }
}

fn spawn_failure(error: &exec::ExecError) -> (Value, bool) {
    let failure = Failure::new(FailureKind::Crash, error.to_string());
    (
        json!({ "success": false, "failures": [failure], "stdout": "", "stderr": "" }),
        true,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_of(stdout: &str, stderr: &str, exit_code: Option<i32>, timed_out: bool) -> Run {
        Run {
            command: vec!["cargo".to_owned()],
            cwd: ".".to_owned(),
            exit_code,
            timed_out,
            duration_seconds: 0.1,
            stdout: stdout.to_owned(),
            stderr: stderr.to_owned(),
        }
    }

    /// A panic and a crash arrive with the same status; the output tells them
    /// apart.
    #[test]
    fn a_panic_is_not_reported_as_a_bare_crash() {
        let run = run_of(
            "",
            "thread 'main' panicked at src/lib.rs:3:1",
            Some(101),
            false,
        );
        assert_eq!(classify(&run), FailureKind::Panic);
        assert!(summary(&run).contains("panicked at"));
    }

    #[test]
    fn a_compile_error_is_reported_as_a_diagnostic() {
        let run = run_of("", "error[E0308]: mismatched types", Some(1), false);
        assert_eq!(classify(&run), FailureKind::Diagnostic);
    }

    #[test]
    fn a_timeout_outranks_whatever_was_printed() {
        let run = run_of("", "thread 'main' panicked at x", None, true);
        assert_eq!(classify(&run), FailureKind::Timeout);
    }

    #[test]
    fn a_backtrace_is_carried_into_the_failure() {
        let run = run_of(
            "",
            "panicked at x\nstack backtrace:\n   0: core::panicking\n",
            Some(101),
            false,
        );
        let text = backtrace_of(&run).expect("a backtrace");
        assert!(text.starts_with("stack backtrace:"));
        assert!(text.contains("core::panicking"));
    }

    /// Naming nothing to run is refused rather than defaulted to something.
    #[test]
    fn a_subject_that_names_nothing_is_refused() {
        assert!(debug(&json!({ "backend": "vm" })).1);
        assert!(reproduce(&json!({ "backend": "vm" })).1);
    }

    #[test]
    fn an_attempt_count_beyond_the_bound_is_refused() {
        let (_, is_error) = reproduce(&json!({ "test": "x", "attempts": MAX_ATTEMPTS + 1 }));
        assert!(is_error);
    }
}
