//! `kira_dev_build`: build Kira itself.

use serde_json::{Value, json};

use super::{bool_field, enum_field, environment, string_field, string_list, timeout};
use crate::exec::{self, Run};
use crate::schema::{Diagnostic, Failure, FailureKind};

/// The scopes a build can be narrowed to, and the crates each names.
///
/// A scope is a *component* of the toolchain, not a cargo concept: an agent
/// changing the VM asks for `vm` rather than remembering which crates that is.
const SCOPES: [&str; 8] = [
    "workspace",
    "crate",
    "package",
    "compiler",
    "runtime",
    "vm",
    "llvm_backend",
    "toolchain",
];

const PROFILES: [&str; 3] = ["debug", "release", "profiling"];

/// The crates each component scope builds.
fn crates_for(scope: &str) -> &'static [&'static str] {
    match scope {
        "compiler" => &[
            "kira-lexer",
            "kira-parser",
            "kira-semantics",
            "kira-ir",
            "kira-bytecode",
        ],
        "runtime" => &["kira-native-bridge", "kira-hybrid-runtime", "kira-main"],
        "vm" => &["kira-vm-runtime", "kira-bytecode"],
        "llvm_backend" => &["kira-llvm-backend"],
        "toolchain" => &["kira-cli", "kira-toolchain", "kira-build", "kira-project"],
        _ => &[],
    }
}

pub fn descriptor() -> Value {
    json!({
        "name": "kira_dev_build",
        "description": "Build the Kira toolchain: the whole workspace, or only the component being modified.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "scope": { "type": "string", "enum": SCOPES },
                "profile": { "type": "string", "enum": PROFILES },
                "crate": { "type": "string" },
                "package": { "type": "string" },
                "target": { "type": "string" },
                "features": { "type": "array", "items": { "type": "string" } },
                "clean": { "type": "boolean" },
                "incremental": { "type": "boolean" },
                "warnings_as_errors": { "type": "boolean" },
                "environment": { "type": "object", "additionalProperties": { "type": "string" } },
                "timeout": { "type": "integer", "minimum": 1 }
            },
            "required": ["scope"]
        }
    })
}

pub fn call(arguments: &Value) -> (Value, bool) {
    let scope = match enum_field(arguments, "scope", &SCOPES, None) {
        Ok(Some(scope)) => scope,
        Ok(None) => return super::invalid("scope", "is required"),
        Err(rejection) => return rejection,
    };
    let profile = match enum_field(arguments, "profile", &PROFILES, Some("debug")) {
        Ok(Some(profile)) => profile,
        Ok(None) => "debug",
        Err(rejection) => return rejection,
    };
    let target = match string_field(arguments, "target") {
        Ok(target) => target,
        Err(rejection) => return rejection,
    };
    let features = match string_list(arguments, "features") {
        Ok(features) => features,
        Err(rejection) => return rejection,
    };
    let mut env = match environment(arguments) {
        Ok(env) => env,
        Err(rejection) => return rejection,
    };
    let bound = match timeout(arguments) {
        Ok(bound) => bound,
        Err(rejection) => return rejection,
    };
    let clean = match bool_field(arguments, "clean", false) {
        Ok(clean) => clean,
        Err(rejection) => return rejection,
    };
    let incremental = match bool_field(arguments, "incremental", true) {
        Ok(incremental) => incremental,
        Err(rejection) => return rejection,
    };
    let strict = match bool_field(arguments, "warnings_as_errors", false) {
        Ok(strict) => strict,
        Err(rejection) => return rejection,
    };

    // The named package for the two scopes that take one.
    let named = match (
        scope,
        string_field(arguments, "crate"),
        string_field(arguments, "package"),
    ) {
        (_, Err(rejection), _) | (_, _, Err(rejection)) => return rejection,
        ("crate", Ok(Some(name)), _) | ("package", Ok(_), Ok(Some(name))) => Some(name.to_owned()),
        ("crate" | "package", _, _) => {
            return super::invalid(scope, "names no crate or package to build");
        }
        _ => None,
    };

    if !incremental {
        env.push(("CARGO_INCREMENTAL".to_owned(), "0".to_owned()));
    }
    if strict {
        env.push(("RUSTFLAGS".to_owned(), "-D warnings".to_owned()));
    }

    let root = exec::repository_root();
    let mut runs: Vec<Run> = Vec::new();

    if clean {
        match exec::run("cargo", &exec::argv(&["clean"]), &root, &env, bound) {
            Ok(run) => runs.push(run),
            Err(error) => return spawn_failure(&error),
        }
    }

    let mut args = vec!["build".to_owned()];
    match (&named, scope) {
        (Some(name), _) => {
            args.push("-p".to_owned());
            args.push(name.clone());
        }
        (None, "workspace") => args.push("--workspace".to_owned()),
        (None, scope) => {
            for name in crates_for(scope) {
                args.push("-p".to_owned());
                args.push((*name).to_owned());
            }
        }
    }
    match profile {
        "release" => args.push("--release".to_owned()),
        // A profiling build is a release build that keeps its symbols, which is
        // what a profiler needs to name a frame.
        "profiling" => {
            args.push("--release".to_owned());
            env.push(("CARGO_PROFILE_RELEASE_DEBUG".to_owned(), "true".to_owned()));
        }
        _ => {}
    }
    if let Some(target) = target {
        args.push("--target".to_owned());
        args.push(target.to_owned());
    }
    if !features.is_empty() {
        args.push("--features".to_owned());
        args.push(features.join(","));
    }
    args.push("--message-format".to_owned());
    args.push("json-diagnostic-rendered-ansi".to_owned());

    let run = match exec::run("cargo", &args, &root, &env, bound) {
        Ok(run) => run,
        Err(error) => return spawn_failure(&error),
    };
    let (diagnostics, artifacts) = parse_cargo_stream(&run.stdout);
    let success = run.success();
    let failures = match success {
        true => Vec::new(),
        false => vec![build_failure(&run)],
    };
    runs.push(run);

    let built: Vec<String> = match (&named, scope) {
        (Some(name), _) => vec![name.clone()],
        (None, "workspace") => vec!["workspace".to_owned()],
        (None, scope) => crates_for(scope).iter().map(|c| (*c).to_owned()).collect(),
    };

    let last = runs.last().expect("the build run is recorded");
    (
        json!({
            "success": success,
            "duration": runs.iter().map(|run| run.duration_seconds).sum::<f64>(),
            "built_components": built,
            "diagnostics": diagnostics,
            "artifacts": artifacts,
            "commands": runs.iter().map(exec::run_json).collect::<Vec<_>>(),
            "failures": failures,
            "stdout": last.stdout,
            "stderr": last.stderr,
        }),
        !success,
    )
}

/// Turns a spawn error into the shared failure shape.
fn spawn_failure(error: &exec::ExecError) -> (Value, bool) {
    let failure = Failure::new(FailureKind::Crash, error.to_string());
    (
        json!({
            "success": false,
            "failures": [failure],
            "diagnostics": [Diagnostic::message("error", error.to_string())],
            "stdout": "",
            "stderr": "",
        }),
        true,
    )
}

/// The failure a non-zero build reports.
fn build_failure(run: &Run) -> Failure {
    let kind = match run.timed_out {
        true => FailureKind::Timeout,
        false => FailureKind::Diagnostic,
    };
    Failure::new(kind, "the build did not succeed").with_run(run)
}

/// Reads cargo's JSON message stream into diagnostics and artifact paths.
///
/// Parsed rather than scraped from rendered text: cargo's `--message-format
/// json` carries the span, the code and the level as fields, and a regex over
/// the human rendering would lose all three the first time cargo reworded
/// anything.
fn parse_cargo_stream(stdout: &str) -> (Vec<Diagnostic>, Vec<String>) {
    let mut diagnostics = Vec::new();
    let mut artifacts = Vec::new();
    for line in stdout.lines() {
        let Ok(message): Result<Value, _> = serde_json::from_str(line) else {
            continue;
        };
        match message["reason"].as_str() {
            Some("compiler-message") => {
                if let Some(diagnostic) = cargo_diagnostic(&message["message"]) {
                    diagnostics.push(diagnostic);
                }
            }
            Some("compiler-artifact") => {
                if let Some(files) = message["filenames"].as_array() {
                    artifacts.extend(
                        files
                            .iter()
                            .filter_map(|file| file.as_str().map(str::to_owned)),
                    );
                }
            }
            _ => {}
        }
    }
    (diagnostics, artifacts)
}

/// One cargo diagnostic in the shared shape.
fn cargo_diagnostic(message: &Value) -> Option<Diagnostic> {
    let text = message["message"].as_str()?;
    let level = message["level"].as_str().unwrap_or("error");
    let primary = message["spans"]
        .as_array()
        .and_then(|spans| spans.iter().find(|span| span["is_primary"] == json!(true)));
    let (file, span) = match primary {
        Some(primary) => (
            primary["file_name"].as_str().map(str::to_owned),
            Some(crate::schema::Span {
                start_line: primary["line_start"].as_u64().unwrap_or_default() as u32,
                start_column: primary["column_start"].as_u64().unwrap_or_default() as u32,
                end_line: primary["line_end"].as_u64().unwrap_or_default() as u32,
                end_column: primary["column_end"].as_u64().unwrap_or_default() as u32,
            }),
        ),
        None => (None, None),
    };
    let children = message["children"].as_array().cloned().unwrap_or_default();
    let notes = children
        .iter()
        .filter_map(|child| child["message"].as_str().map(str::to_owned))
        .collect();
    let suggestions = children
        .iter()
        .filter_map(|child| child["spans"].as_array())
        .flatten()
        .filter_map(|span| span["suggested_replacement"].as_str().map(str::to_owned))
        .collect();
    Some(Diagnostic {
        severity: level.to_owned(),
        code: message["code"]["code"].as_str().map(str::to_owned),
        message: text.to_owned(),
        file,
        span,
        notes,
        suggestions,
        related_diagnostics: Vec::new(),
        compiler_stage: Some("rustc".to_owned()),
        backend: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_scope_that_names_no_crate_is_refused() {
        let (value, is_error) = call(&json!({ "scope": "crate" }));
        assert!(is_error);
        assert_eq!(value["success"], json!(false));
    }

    #[test]
    fn an_unknown_scope_is_refused_rather_than_treated_as_the_workspace() {
        let (_, is_error) = call(&json!({ "scope": "everything" }));
        assert!(is_error);
    }

    /// A component scope names real crates, so a build of it builds something.
    #[test]
    fn every_component_scope_names_at_least_one_crate() {
        for scope in ["compiler", "runtime", "vm", "llvm_backend", "toolchain"] {
            assert!(!crates_for(scope).is_empty(), "`{scope}` names no crate");
        }
    }

    /// Cargo's JSON carries the code and span; a rendered-text scrape would not.
    #[test]
    fn a_cargo_diagnostic_keeps_its_code_and_span() {
        let stream = json!({
            "reason": "compiler-message",
            "message": {
                "message": "mismatched types",
                "level": "error",
                "code": { "code": "E0308" },
                "spans": [{
                    "is_primary": true,
                    "file_name": "src/lib.rs",
                    "line_start": 3, "line_end": 3,
                    "column_start": 5, "column_end": 9
                }],
                "children": []
            }
        })
        .to_string();
        let (diagnostics, _) = parse_cargo_stream(&stream);
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].code.as_deref(), Some("E0308"));
        assert_eq!(diagnostics[0].file.as_deref(), Some("src/lib.rs"));
        let span = diagnostics[0].span.as_ref().expect("a primary span");
        assert_eq!((span.start_line, span.start_column), (3, 5));
    }

    /// A line cargo did not write as JSON is skipped, not guessed at.
    #[test]
    fn non_json_output_is_ignored_rather_than_misparsed() {
        let (diagnostics, artifacts) = parse_cargo_stream("warning: something\nnot json at all\n");
        assert!(diagnostics.is_empty());
        assert!(artifacts.is_empty());
    }
}
