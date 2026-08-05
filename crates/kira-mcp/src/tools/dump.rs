//! `kira_dev_dump`: the compiler's own view of a program at one stage.

use serde_json::{Value, json};

use super::{BACKENDS, DEVICES, enum_field, environment, string_field, timeout};
use crate::exec;
use crate::schema::{Failure, FailureKind, capability_missing};

/// The stages a caller can ask for.
///
/// Listed whole, including the ones this repository has no dumper for: a stage
/// missing from the schema reads as "Kira has no such stage", which is a
/// different and wrong answer from "nothing emits it yet".
const STAGES: [&str; 8] = [
    "tokens", "ast", "hir", "mid_ir", "ir", "bytecode", "llvm_ir", "asm",
];

pub fn descriptor() -> Value {
    json!({
        "name": "kira_dev_dump",
        "description": "Dump the compiler's intermediate representation of a Kira program at one stage: tokens, AST, or LLVM IR.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "stage": { "type": "string", "enum": STAGES },
                "source": { "type": "string" },
                "file": { "type": "string" },
                "backend": { "type": "string", "enum": BACKENDS },
                "device": { "type": "string", "enum": DEVICES },
                "environment": { "type": "object", "additionalProperties": { "type": "string" } },
                "timeout": { "type": "integer", "minimum": 1 }
            },
            "required": ["stage"]
        }
    })
}

pub fn call(arguments: &Value) -> (Value, bool) {
    let stage = match enum_field(arguments, "stage", &STAGES, None) {
        Ok(Some(stage)) => stage,
        Ok(None) => return super::invalid("stage", "is required"),
        Err(rejection) => return rejection,
    };
    let device = match enum_field(arguments, "device", &DEVICES, Some("host")) {
        Ok(device) => device.unwrap_or("host"),
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

    // The stages the CLI can actually emit today.
    let mut args = match stage {
        "tokens" => vec!["tokens".to_owned()],
        "ast" => vec!["ast".to_owned()],
        "llvm_ir" => exec::argv(&["build", "--backend", "llvm", "--emit-llvm-ir"]),
        stage => {
            return capability_missing(
                &format!("dump:{stage}"),
                &format!(
                    "the toolchain has no dumper for the `{stage}` stage; \
                     `tokens`, `ast` and `llvm_ir` are the stages it can emit"
                ),
            );
        }
    };
    if device != "host" {
        args.push("--device".to_owned());
        args.push(device.to_owned());
    }
    args.push(source.clone());

    let root = exec::repository_root();
    let mut invocation = exec::argv(&["run", "-q", "-p", "kira-cli", "--"]);
    invocation.extend(args);

    let run = match exec::run("cargo", &invocation, &root, &env, bound) {
        Ok(run) => run,
        Err(error) => {
            let failure = Failure::new(FailureKind::Crash, error.to_string());
            return (
                json!({ "success": false, "failures": [failure], "stdout": "", "stderr": "" }),
                true,
            );
        }
    };

    let success = run.success();
    // A dump that produced nothing is not a dump, whatever the exit code said.
    let empty = run.stdout.trim().is_empty();
    let failures = match (success, empty, stage) {
        (true, false, _) => Vec::new(),
        (true, true, "llvm_ir") => vec![
            Failure::new(
                FailureKind::InvalidArtifact,
                "the build succeeded but wrote no LLVM IR to stdout; \
             read `stderr` for where it was written instead",
            )
            .with_run(&run),
        ],
        (true, true, _) => vec![
            Failure::new(
                FailureKind::InvalidArtifact,
                format!("the `{stage}` dump produced no output"),
            )
            .with_run(&run),
        ],
        (false, _, _) => vec![
            Failure::new(
                match run.timed_out {
                    true => FailureKind::Timeout,
                    false => FailureKind::Diagnostic,
                },
                format!("the `{stage}` dump did not succeed"),
            )
            .with_run(&run),
        ],
    };

    let ok = failures.is_empty();
    (
        json!({
            "success": ok,
            "stage": stage,
            "source": source,
            "device": device,
            "representation": run.stdout,
            "failures": failures,
            "commands": [exec::run_json(&run)],
            "stdout": run.stdout,
            "stderr": run.stderr,
        }),
        !ok,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stage with no dumper says so, rather than returning an empty dump.
    #[test]
    fn a_stage_with_no_dumper_reports_a_missing_capability() {
        let (value, is_error) = call(&json!({ "stage": "bytecode", "source": "m.kira" }));
        assert!(is_error);
        assert_eq!(value["failures"][0]["kind"], json!("capability_missing"));
    }

    #[test]
    fn a_dump_without_a_source_is_refused() {
        let (_, is_error) = call(&json!({ "stage": "ast" }));
        assert!(is_error);
    }

    #[test]
    fn an_unknown_stage_is_refused() {
        let (_, is_error) = call(&json!({ "stage": "trees", "source": "m.kira" }));
        assert!(is_error);
    }
}
