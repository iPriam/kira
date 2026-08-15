//! Starting, listing, inspecting, and ending debug sessions.

use serde_json::{Value, json};

use super::{bool_field, descriptor, enum_field, session_field, session_property, string_list};
use crate::build::{Program, prepare};
use crate::registry::Sessions;
use crate::report::stop_report;
use crate::session::LAUNCH_TIMEOUT;

/// The backends a Kira program can be debugged on.
const BACKENDS: [&str; 3] = ["vm", "llvm", "hybrid"];

/// The lifecycle tools.
pub fn descriptors() -> Vec<Value> {
    vec![
        descriptor(
            "kira_lldb_launch",
            "Build a Kira program and start a debug session over it under LLDB. \
             Give `source` for a .kira file or package directory, or `executable` for a \
             binary that is already built. Returns the session identifier, the target's \
             Kira function table, and where it first stopped. Without `breakpoints` the \
             program runs to completion, and the report carries its output and exit code.",
            json!({
                "source": {
                    "type": "string",
                    "description": "A .kira file or package directory to build and debug.",
                },
                "executable": {
                    "type": "string",
                    "description": "An already-built executable to debug instead of building one.",
                },
                "backend": {
                    "type": "string",
                    "enum": BACKENDS,
                    "description": "The backend to build for. Defaults to vm.",
                },
                "release": {
                    "type": "boolean",
                    "description": "Build the native unit optimized. Defaults to false.",
                },
                "breakpoints": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Kira breakpoints as `function` or `function:instruction`, \
                                    placed before the program starts.",
                },
                "arguments": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Arguments passed to the debugged program.",
                },
                "stop": {
                    "type": "boolean",
                    "description": "Wait for the first stop before answering. Defaults to true.",
                },
            }),
            &[],
        ),
        descriptor(
            "kira_lldb_sessions",
            "List every open debug session, what it is debugging, and what it is doing.",
            json!({}),
            &[],
        ),
        descriptor(
            "kira_lldb_status",
            "Report where a session is: its state, the stopped frame, the decoded Kira \
             stop, and anything the program has printed since the last call.",
            json!({ "session": session_property() }),
            &[],
        ),
        descriptor(
            "kira_lldb_close",
            "End a debug session, kill the target, and delete the artifacts it owned.",
            json!({ "session": session_property() }),
            &[],
        ),
    ]
}

/// Builds a target, starts a session over it, and reports the first stop.
pub fn launch(sessions: &mut Sessions, arguments: &Value) -> Result<Value, String> {
    let source = arguments["source"].as_str();
    let executable = arguments["executable"].as_str();
    let backend = enum_field(arguments, "backend", &BACKENDS, "vm")?;
    let release = bool_field(arguments, "release", false)?;
    let wait = bool_field(arguments, "stop", true)?;
    let breakpoints = string_list(arguments, "breakpoints")?;
    let program_arguments = string_list(arguments, "arguments")?;

    let program = match (source, executable) {
        (Some(path), None) => Program::Source {
            path,
            backend,
            release,
            breakpoints: &breakpoints,
            arguments: &program_arguments,
        },
        (None, Some(path)) => Program::Executable {
            path,
            arguments: &program_arguments,
        },
        (Some(_), Some(_)) => {
            return Err("give `source` or `executable`, not both".to_owned());
        }
        (None, None) => {
            return Err("`source` (a .kira path) or `executable` is required".to_owned());
        }
    };
    let target = prepare(&program)?;
    let functions = target.functions.clone();
    let session = sessions.open(target)?;

    // The Kira breakpoints the build already honours are recorded here too, so
    // `kira_lldb_break_list` shows what the session is actually stopping at
    // rather than an empty list beside a target that stops.
    for spelling in &breakpoints {
        let placement = super::breakpoints::kira_placement(session, spelling)?;
        session.add_breakpoint(placement, None)?;
    }

    let mut report = match wait {
        true => {
            session.await_stop(LAUNCH_TIMEOUT)?;
            stop_report(session)
        }
        false => stop_report(session),
    };
    report["functions"] = serde_json::to_value(functions).unwrap_or(Value::Null);
    Ok(report)
}

/// Lists the open sessions.
pub fn list(sessions: &mut Sessions) -> Result<Value, String> {
    Ok(json!({ "sessions": sessions.summaries() }))
}

/// Reports where one session is.
pub fn status(sessions: &mut Sessions, arguments: &Value) -> Result<Value, String> {
    let session = sessions.select(session_field(arguments))?;
    Ok(stop_report(session))
}

/// Ends one session.
pub fn close(sessions: &mut Sessions, arguments: &Value) -> Result<Value, String> {
    let id = match session_field(arguments) {
        Some(id) => id.to_owned(),
        None => sessions.only()?.id.clone(),
    };
    sessions.close(&id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launching_without_a_program_says_which_field_is_missing() {
        let mut sessions = Sessions::default();
        let error = launch(&mut sessions, &json!({})).expect_err("no program");
        assert!(error.contains("`source`"), "error was: {error}");
        assert!(error.contains("executable"), "error was: {error}");
    }

    /// Two programs in one call is ambiguous, and picking one would debug
    /// something the caller did not ask for.
    #[test]
    fn launching_with_both_a_source_and_an_executable_is_refused() {
        let mut sessions = Sessions::default();
        let arguments = json!({ "source": "demo.kira", "executable": "demo.exe" });
        let error = launch(&mut sessions, &arguments).expect_err("ambiguous");
        assert!(error.contains("not both"), "error was: {error}");
    }

    #[test]
    fn an_unknown_backend_is_refused_before_anything_is_built() {
        let mut sessions = Sessions::default();
        let arguments = json!({ "source": "demo.kira", "backend": "wasm" });
        assert!(launch(&mut sessions, &arguments).is_err());
    }

    #[test]
    fn listing_with_nothing_open_reports_an_empty_list_rather_than_failing() {
        let mut sessions = Sessions::default();
        let result = list(&mut sessions).expect("a list");
        assert_eq!(result["sessions"], json!([]));
    }

    #[test]
    fn closing_with_nothing_open_names_the_tool_that_opens_one() {
        let mut sessions = Sessions::default();
        let error = close(&mut sessions, &json!({})).expect_err("nothing to close");
        assert!(error.contains("kira_lldb_launch"), "error was: {error}");
    }
}
