//! `kira_dev_compare`: where two engines stop agreeing.
//!
//! Kira promises the same program means the same thing on the VM, on native
//! code and in a hybrid process. This tool is how that promise is checked:
//! it runs one program under two configurations and reports the first place
//! their observable behaviour differs.

use serde_json::{Value, json};

use super::program::{self, Configuration};
use super::{BACKENDS, DEVICES, enum_field, environment, string_field, string_list, timeout};
use crate::exec::{self, Run};
use crate::schema::{Failure, FailureKind};

pub fn descriptor() -> Value {
    json!({
        "name": "kira_dev_compare",
        "description": "Run one Kira program under two backends or devices and report the first observable difference between them.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "source": { "type": "string" },
                "file": { "type": "string" },
                "arguments": { "type": "array", "items": { "type": "string" } },
                "backend": { "type": "string", "enum": BACKENDS },
                "against": { "type": "string", "enum": BACKENDS },
                "device": { "type": "string", "enum": DEVICES },
                "against_device": { "type": "string", "enum": DEVICES },
                "environment": { "type": "object", "additionalProperties": { "type": "string" } },
                "timeout": { "type": "integer", "minimum": 1 }
            },
            "required": ["source"]
        }
    })
}

pub fn call(arguments: &Value) -> (Value, bool) {
    let source = match (
        string_field(arguments, "source"),
        string_field(arguments, "file"),
    ) {
        (Err(rejection), _) | (_, Err(rejection)) => return rejection,
        (Ok(source), Ok(file)) => match source.or(file) {
            Some(source) => source.to_owned(),
            None => return super::invalid("source", "a Kira source file is required"),
        },
    };
    let left_backend = match enum_field(arguments, "backend", &BACKENDS, Some("vm")) {
        Ok(backend) => backend.unwrap_or("vm"),
        Err(rejection) => return rejection,
    };
    let right_backend = match enum_field(arguments, "against", &BACKENDS, Some("llvm")) {
        Ok(backend) => backend.unwrap_or("llvm"),
        Err(rejection) => return rejection,
    };
    let left_device = match enum_field(arguments, "device", &DEVICES, Some("host")) {
        Ok(device) => device.unwrap_or("host"),
        Err(rejection) => return rejection,
    };
    let right_device = match enum_field(arguments, "against_device", &DEVICES, Some(left_device)) {
        Ok(device) => device.unwrap_or(left_device),
        Err(rejection) => return rejection,
    };
    let program_args = match string_list(arguments, "arguments") {
        Ok(args) => args,
        Err(rejection) => return rejection,
    };
    let env = match environment(arguments) {
        Ok(env) => env,
        Err(rejection) => return rejection,
    };
    let bound = match timeout(arguments) {
        Ok(bound) => bound,
        Err(rejection) => return rejection,
    };

    let left = Configuration::new(left_backend, left_device);
    let right = Configuration::new(right_backend, right_device);
    if left == right {
        return super::invalid(
            "against",
            "the two configurations are identical, so there is nothing to compare",
        );
    }

    let mut runs: Vec<Run> = Vec::new();
    for configuration in [&left, &right] {
        match program::run(configuration, &source, &program_args, &env, bound) {
            Ok(run) => runs.push(run),
            Err(error) => {
                let failure = Failure::new(FailureKind::Crash, error.to_string());
                return (
                    json!({ "success": false, "failures": [failure], "stdout": "", "stderr": "" }),
                    true,
                );
            }
        }
    }

    let differences = differences(&left, &runs[0], &right, &runs[1]);
    let agreed = differences.is_empty();
    let failures: Vec<Failure> = differences
        .iter()
        .map(|difference| {
            // One engine finishing while the other does not is a hang, and
            // naming it one tells a caller to look for a loop rather than for a
            // wrong value.
            let kind = match difference["kind"].as_str() {
                Some("termination") => FailureKind::Hang,
                _ => FailureKind::BackendDivergence,
            };
            let mut failure = Failure::new(
                kind,
                difference["summary"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned(),
            );
            failure.backend = Some(format!("{} vs {}", left.label(), right.label()));
            failure.command = Some(runs[0].command.clone());
            failure.stdout = runs[0].stdout.clone();
            failure.stderr = runs[1].stdout.clone();
            failure
        })
        .collect();

    (
        json!({
            "success": agreed,
            "identical": agreed,
            "configurations": [left.json(), right.json()],
            "differences": differences,
            "failures": failures,
            "commands": runs.iter().map(exec::run_json).collect::<Vec<_>>(),
            "stdout": runs[0].stdout,
            "stderr": runs[0].stderr,
        }),
        !agreed,
    )
}

/// Every way the two runs disagreed, most significant first.
///
/// A crash on one side and a clean exit on the other is reported as a
/// divergence in its own right, before the output diff: two runs that printed
/// nothing agree on their output while disagreeing about everything else.
fn differences(
    left: &Configuration,
    left_run: &Run,
    right: &Configuration,
    right_run: &Run,
) -> Vec<Value> {
    let mut differences = Vec::new();

    if left_run.timed_out != right_run.timed_out {
        let hung = match left_run.timed_out {
            true => left,
            false => right,
        };
        differences.push(json!({
            "kind": "termination",
            "summary": format!("`{}` did not terminate within the timeout while the other did", hung.label()),
            "left": left_run.timed_out,
            "right": right_run.timed_out,
        }));
    }

    if left_run.exit_code != right_run.exit_code {
        differences.push(json!({
            "kind": "exit_code",
            "summary": format!(
                "`{}` exited {:?} and `{}` exited {:?}",
                left.label(), left_run.exit_code, right.label(), right_run.exit_code
            ),
            "left": left_run.exit_code,
            "right": right_run.exit_code,
        }));
    }

    if left_run.stdout != right_run.stdout {
        let line = first_differing_line(&left_run.stdout, &right_run.stdout);
        let number = line.as_ref().map(|(index, _, _)| index + 1);
        differences.push(json!({
            "kind": "stdout",
            "summary": format!(
                "the two runs printed different output, first at line {}",
                number.unwrap_or(0)
            ),
            "line": number,
            "left": line.as_ref().map(|(_, left, _)| left.clone()),
            "right": line.as_ref().map(|(_, _, right)| right.clone()),
        }));
    }

    differences
}

/// The first line at which two outputs differ, with both sides.
///
/// Reported by line rather than as a whole-text inequality so a caller sees
/// *where* the engines parted, which is where the bug is.
fn first_differing_line(left: &str, right: &str) -> Option<(usize, String, String)> {
    let mut left_lines = left.lines();
    let mut right_lines = right.lines();
    let mut index = 0;
    loop {
        match (left_lines.next(), right_lines.next()) {
            (None, None) => return None,
            (left_line, right_line) => {
                let left_line = left_line.unwrap_or_default();
                let right_line = right_line.unwrap_or_default();
                if left_line != right_line {
                    return Some((index, left_line.to_owned(), right_line.to_owned()));
                }
            }
        }
        index += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_of(stdout: &str, exit_code: Option<i32>, timed_out: bool) -> Run {
        Run {
            command: vec!["cargo".to_owned()],
            cwd: ".".to_owned(),
            exit_code,
            timed_out,
            duration_seconds: 0.5,
            stdout: stdout.to_owned(),
            stderr: String::new(),
        }
    }

    #[test]
    fn two_agreeing_runs_report_no_difference() {
        let differences = differences(
            &Configuration::new("vm", "host"),
            &run_of("42\n", Some(0), false),
            &Configuration::new("llvm", "host"),
            &run_of("42\n", Some(0), false),
        );
        assert!(differences.is_empty());
    }

    /// The line number points at where the engines parted.
    #[test]
    fn a_divergence_names_the_first_line_that_differs() {
        let differences = differences(
            &Configuration::new("vm", "host"),
            &run_of("a\nb\nc\n", Some(0), false),
            &Configuration::new("llvm", "host"),
            &run_of("a\nX\nc\n", Some(0), false),
        );
        assert_eq!(differences.len(), 1);
        assert_eq!(differences[0]["kind"], json!("stdout"));
        assert_eq!(differences[0]["line"], json!(2));
        assert_eq!(differences[0]["left"], json!("b"));
        assert_eq!(differences[0]["right"], json!("X"));
    }

    /// Identical output with different exit codes is still a divergence.
    #[test]
    fn a_differing_exit_code_is_a_divergence_even_with_equal_output() {
        let differences = differences(
            &Configuration::new("vm", "host"),
            &run_of("", Some(0), false),
            &Configuration::new("llvm", "host"),
            &run_of("", Some(134), false),
        );
        assert_eq!(differences.len(), 1);
        assert_eq!(differences[0]["kind"], json!("exit_code"));
    }

    /// One side hanging is reported before any output comparison.
    #[test]
    fn a_hang_on_one_side_is_reported_first() {
        let differences = differences(
            &Configuration::new("vm", "host"),
            &run_of("", None, true),
            &Configuration::new("llvm", "host"),
            &run_of("", Some(0), false),
        );
        assert_eq!(differences[0]["kind"], json!("termination"));
    }

    #[test]
    fn comparing_a_configuration_against_itself_is_refused() {
        let (_, is_error) = call(&json!({
            "source": "m.kira", "backend": "vm", "against": "vm"
        }));
        assert!(is_error);
    }

    /// A trailing extra line on one side is a difference, not a match.
    #[test]
    fn extra_output_on_one_side_differs() {
        let line = first_differing_line("a\n", "a\nb\n").expect("a difference");
        assert_eq!(line, (1, String::new(), "b".to_owned()));
    }
}
