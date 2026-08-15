//! The tools this server offers, their schemas, and dispatch.
//!
//! The surface is the one a debugger frontend has: start a session, place
//! breakpoints, resume, step, and read the stopped state — plus the three
//! things only Kira can answer, which are the decoded VM stop, the Kira
//! function table a breakpoint resolves against, and the source the stopped
//! bytecode came from.

use serde_json::{Value, json};

use crate::registry::Sessions;

mod breakpoints;
mod code;
mod execution;
mod inspect;
mod kira;
mod lifecycle;

/// Every tool this server offers, as MCP tool descriptors.
pub fn descriptors() -> Value {
    let mut tools = Vec::new();
    tools.extend(lifecycle::descriptors());
    tools.extend(breakpoints::descriptors());
    tools.extend(execution::descriptors());
    tools.extend(inspect::descriptors());
    tools.extend(kira::descriptors());
    tools.extend(code::descriptors());
    Value::Array(tools)
}

/// Runs one tool, returning its structured result and whether it failed.
pub fn call(sessions: &mut Sessions, name: &str, arguments: &Value) -> Option<(Value, bool)> {
    let result = match name {
        "kira_lldb_launch" => lifecycle::launch(sessions, arguments),
        "kira_lldb_sessions" => lifecycle::list(sessions),
        "kira_lldb_status" => lifecycle::status(sessions, arguments),
        "kira_lldb_close" => lifecycle::close(sessions, arguments),
        "kira_lldb_break_set" => breakpoints::set(sessions, arguments),
        "kira_lldb_break_list" => breakpoints::list(sessions, arguments),
        "kira_lldb_break_delete" => breakpoints::delete(sessions, arguments),
        "kira_lldb_watch" => breakpoints::watch(sessions, arguments),
        "kira_lldb_continue" => execution::resume(sessions, arguments),
        "kira_lldb_step" => execution::step(sessions, arguments),
        "kira_lldb_pause" => execution::pause(sessions, arguments),
        "kira_lldb_finish" => execution::finish(sessions, arguments),
        "kira_lldb_backtrace" => inspect::backtrace(sessions, arguments),
        "kira_lldb_variables" => inspect::variables(sessions, arguments),
        "kira_lldb_evaluate" => inspect::evaluate(sessions, arguments),
        "kira_lldb_registers" => inspect::registers(sessions, arguments),
        "kira_lldb_read_memory" => inspect::read_memory(sessions, arguments),
        "kira_lldb_write_memory" => inspect::write_memory(sessions, arguments),
        "kira_lldb_threads" => inspect::threads(sessions, arguments),
        "kira_lldb_state" => kira::state(sessions, arguments),
        "kira_lldb_functions" => kira::functions(sessions, arguments),
        "kira_lldb_source" => kira::source(sessions, arguments),
        "kira_lldb_disassemble" => code::disassemble(sessions, arguments),
        "kira_lldb_modules" => code::modules(sessions, arguments),
        "kira_lldb_command" => code::command(sessions, arguments),
        _ => return None,
    };
    Some(match result {
        Ok(mut value) => {
            value["success"] = json!(true);
            (value, false)
        }
        Err(message) => (json!({ "success": false, "error": message }), true),
    })
}

/// Builds one tool descriptor.
pub fn descriptor(name: &str, description: &str, properties: Value, required: &[&str]) -> Value {
    json!({
        "name": name,
        "description": description,
        "inputSchema": {
            "type": "object",
            "properties": properties,
            "required": required,
        },
    })
}

/// The `session` property every tool accepts.
pub fn session_property() -> Value {
    json!({
        "type": "string",
        "description": "The session to act on. Optional while exactly one is open.",
    })
}

/// Reads the optional `session` field.
pub fn session_field(arguments: &Value) -> Option<&str> {
    arguments["session"].as_str()
}

/// Reads a string field that must be present.
pub fn required_string<'a>(arguments: &'a Value, field: &str) -> Result<&'a str, String> {
    arguments[field]
        .as_str()
        .ok_or_else(|| format!("`{field}` is required and must be a string"))
}

/// Reads a string field, defaulting when absent.
pub fn string_field<'a>(arguments: &'a Value, field: &str, default: &'a str) -> &'a str {
    arguments[field].as_str().unwrap_or(default)
}

/// Reads an integer field, defaulting when absent.
pub fn uint_field(arguments: &Value, field: &str, default: u64) -> Result<u64, String> {
    match &arguments[field] {
        Value::Null => Ok(default),
        Value::Number(number) => number
            .as_u64()
            .ok_or_else(|| format!("`{field}` must be a non-negative integer")),
        _ => Err(format!("`{field}` must be a number")),
    }
}

/// Reads a boolean field, defaulting when absent.
pub fn bool_field(arguments: &Value, field: &str, default: bool) -> Result<bool, String> {
    match &arguments[field] {
        Value::Null => Ok(default),
        Value::Bool(value) => Ok(*value),
        _ => Err(format!("`{field}` must be a boolean")),
    }
}

/// Reads a string-array field.
pub fn string_list(arguments: &Value, field: &str) -> Result<Vec<String>, String> {
    match &arguments[field] {
        Value::Null => Ok(Vec::new()),
        Value::Array(items) => items
            .iter()
            .map(|item| {
                item.as_str()
                    .map(str::to_owned)
                    .ok_or_else(|| format!("`{field}` must be an array of strings"))
            })
            .collect(),
        _ => Err(format!("`{field}` must be an array of strings")),
    }
}

/// Reads an enumerated string field, refusing a value outside `allowed`.
///
/// An unknown value is refused rather than defaulted: a `backend` of `"llvm2"`
/// that silently became `llvm` would answer confidently about a configuration
/// the caller never asked for.
pub fn enum_field<'a>(
    arguments: &'a Value,
    field: &str,
    allowed: &[&str],
    default: &'a str,
) -> Result<&'a str, String> {
    match &arguments[field] {
        Value::Null => Ok(default),
        Value::String(text) => match allowed.contains(&text.as_str()) {
            true => Ok(text),
            false => Err(format!("`{field}` must be one of {allowed:?}")),
        },
        _ => Err(format!("`{field}` must be a string")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every advertised tool must dispatch, and nothing else may.
    #[test]
    fn the_advertised_surface_is_exactly_the_dispatched_one() {
        let mut sessions = Sessions::default();
        let advertised = descriptors();
        let names = advertised
            .as_array()
            .expect("an array")
            .iter()
            .map(|tool| tool["name"].as_str().expect("a name").to_owned())
            .collect::<Vec<_>>();
        assert_eq!(names.len(), 25, "the surface is 25 tools: {names:?}");
        for name in &names {
            assert!(
                call(&mut sessions, name, &json!({})).is_some(),
                "`{name}` is advertised but does not dispatch"
            );
        }
        assert!(call(&mut sessions, "kira_lldb_absent", &json!({})).is_none());
    }

    #[test]
    fn every_tool_is_named_described_and_given_an_object_schema() {
        for tool in descriptors().as_array().expect("an array") {
            assert!(tool["name"].is_string(), "every tool is named");
            assert!(tool["description"].is_string(), "every tool is described");
            assert_eq!(tool["inputSchema"]["type"], json!("object"));
            assert!(tool["inputSchema"]["required"].is_array());
        }
    }

    /// A tool called with no session open fails as a result, not as a crash.
    #[test]
    fn a_tool_without_a_session_answers_with_a_failed_result() {
        let mut sessions = Sessions::default();
        let (value, is_error) =
            call(&mut sessions, "kira_lldb_backtrace", &json!({})).expect("a result");
        assert!(is_error);
        assert_eq!(value["success"], json!(false));
        assert!(value["error"].is_string());
    }

    #[test]
    fn an_unknown_enumerated_value_is_refused_rather_than_defaulted() {
        let arguments = json!({ "backend": "llvm2" });
        assert!(enum_field(&arguments, "backend", &["vm", "llvm"], "vm").is_err());
        assert_eq!(
            enum_field(&json!({}), "backend", &["vm", "llvm"], "vm"),
            Ok("vm")
        );
    }

    #[test]
    fn a_required_string_is_reported_by_name_when_it_is_missing() {
        let error = required_string(&json!({}), "function").expect_err("missing");
        assert!(error.contains("`function`"), "error was: {error}");
    }

    #[test]
    fn a_list_field_refuses_entries_that_are_not_strings() {
        assert!(string_list(&json!({ "arguments": ["a", 2] }), "arguments").is_err());
        assert_eq!(
            string_list(&json!({ "arguments": ["a"] }), "arguments"),
            Ok(vec!["a".to_owned()])
        );
        assert_eq!(string_list(&json!({}), "arguments"), Ok(Vec::new()));
    }
}
