//! The eight tools, their input schemas, and dispatch.
//!
//! Deliberately eight and no more. This server exposes Kira-specific compiler
//! operations that a shell cannot give reliably: which stage a build reached,
//! where two backends first diverge, what the bytecode for one function is.
//! Searching, reading and editing the repository are not here, because the
//! client already does those better.
//!
//! Running the tests is not one of the eight either. It is a phase of
//! `kira_dev_validate`, because a gate and the tests it depends on being two
//! calls made passing the gate without them possible.

use serde_json::{Value, json};

use crate::exec;
use crate::schema::{Diagnostic, Failure, FailureKind};

mod build;
mod compare;
mod dump;
mod investigate;
mod measure;
mod program;
mod suite;
mod validate;

/// Every tool this server offers, as MCP tool descriptors.
pub fn descriptors() -> Value {
    json!([
        build::descriptor(),
        validate::descriptor(),
        investigate::debug_descriptor(),
        dump::descriptor(),
        compare::descriptor(),
        investigate::reproduce_descriptor(),
        measure::benchmark_descriptor(),
        measure::fuzz_descriptor(),
    ])
}

/// Runs one tool, returning its structured result and whether it failed.
pub fn call(name: &str, arguments: &Value) -> Option<(Value, bool)> {
    Some(match name {
        "kira_dev_build" => build::call(arguments),
        "kira_dev_validate" => validate::call(arguments),
        "kira_dev_debug" => investigate::debug(arguments),
        "kira_dev_dump" => dump::call(arguments),
        "kira_dev_compare" => compare::call(arguments),
        "kira_dev_reproduce" => investigate::reproduce(arguments),
        "kira_dev_benchmark" => measure::benchmark(arguments),
        "kira_dev_fuzz" => measure::fuzz(arguments),
        _ => return None,
    })
}

/// A rejected input, in the shape every tool returns failures in.
///
/// Input validation refuses rather than defaults: a `backend` of `"llvm2"` that
/// silently became `llvm` would answer confidently about a configuration the
/// caller never asked for.
pub fn invalid(field: &str, detail: &str) -> (Value, bool) {
    let failure = Failure::new(
        FailureKind::Diagnostic,
        format!("invalid `{field}`: {detail}"),
    );
    (
        json!({
            "success": false,
            "failures": [failure],
            "diagnostics": [Diagnostic::message("error", format!("invalid `{field}`: {detail}"))],
            "stdout": "",
            "stderr": "",
        }),
        true,
    )
}

/// Reads an enum-valued string field, checking it against `allowed`.
pub fn enum_field<'a>(
    arguments: &'a Value,
    field: &str,
    allowed: &[&str],
    default: Option<&'a str>,
) -> Result<Option<&'a str>, (Value, bool)> {
    match arguments.get(field) {
        None | Some(Value::Null) => Ok(default),
        Some(Value::String(text)) => match allowed.contains(&text.as_str()) {
            true => Ok(Some(text)),
            false => Err(invalid(field, &format!("expected one of {allowed:?}"))),
        },
        Some(_) => Err(invalid(field, "expected a string")),
    }
}

/// Reads a boolean field, defaulting when absent.
pub fn bool_field(arguments: &Value, field: &str, default: bool) -> Result<bool, (Value, bool)> {
    match arguments.get(field) {
        None | Some(Value::Null) => Ok(default),
        Some(Value::Bool(value)) => Ok(*value),
        Some(_) => Err(invalid(field, "expected a boolean")),
    }
}

/// Reads a string field.
pub fn string_field<'a>(
    arguments: &'a Value,
    field: &str,
) -> Result<Option<&'a str>, (Value, bool)> {
    match arguments.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) => Ok(Some(text)),
        Some(_) => Err(invalid(field, "expected a string")),
    }
}

/// Reads a positive integer field.
pub fn uint_field(arguments: &Value, field: &str, default: u64) -> Result<u64, (Value, bool)> {
    match arguments.get(field) {
        None | Some(Value::Null) => Ok(default),
        Some(Value::Number(number)) => match number.as_u64() {
            Some(value) => Ok(value),
            None => Err(invalid(field, "expected a non-negative integer")),
        },
        Some(_) => Err(invalid(field, "expected a number")),
    }
}

/// Reads a string-array field.
pub fn string_list(arguments: &Value, field: &str) -> Result<Vec<String>, (Value, bool)> {
    match arguments.get(field) {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Array(items)) => {
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                match item.as_str() {
                    Some(text) => out.push(text.to_owned()),
                    None => return Err(invalid(field, "expected an array of strings")),
                }
            }
            Ok(out)
        }
        Some(_) => Err(invalid(field, "expected an array of strings")),
    }
}

/// The environment overrides a caller supplied, as pairs.
pub fn environment(arguments: &Value) -> Result<Vec<(String, String)>, (Value, bool)> {
    match arguments.get("environment") {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Object(map)) => {
            let mut out = Vec::with_capacity(map.len());
            for (key, value) in map {
                match value.as_str() {
                    Some(text) => out.push((key.clone(), text.to_owned())),
                    None => return Err(invalid("environment", "every value must be a string")),
                }
            }
            Ok(out)
        }
        Some(_) => Err(invalid("environment", "expected an object of strings")),
    }
}

/// The timeout a caller asked for, or the default.
pub fn timeout(arguments: &Value) -> Result<std::time::Duration, (Value, bool)> {
    let seconds = uint_field(arguments, "timeout", exec::DEFAULT_TIMEOUT_SECONDS)?;
    match seconds {
        0 => Err(invalid("timeout", "must be at least one second")),
        seconds => Ok(std::time::Duration::from_secs(seconds)),
    }
}

/// How much of a run a caller wants back.
pub const DETAILS: [&str; 3] = ["summary", "failures", "full"];

/// Trims a result to the detail level asked for.
///
/// The whole result is saved to a run first and the identifier is carried in
/// `session`, so trimming here loses nothing permanently: what a summary leaves
/// out, `kira_dev_debug` reads back without running anything again.
///
/// Nothing is trimmed away *silently*. A summary still carries one entry per
/// failure, with its kind and message, because a caller that read an empty
/// `failures` array would conclude the run passed.
pub fn project(detail: &str, mut value: Value) -> Value {
    if detail == "full" {
        value["detail"] = json!(detail);
        return value;
    }
    let keep_evidence = detail == "failures";
    // `failures` and `violations` both carry whole command transcripts. At
    // the failures level a bounded tail stays — the end of a compiler or test
    // run is where its verdict is — and a summary keeps the message alone.
    // Either way the untrimmed run is in the session, one read away.
    for key in ["failures", "violations"] {
        if let Some(entries) = value[key].as_array_mut() {
            for failure in entries {
                for stream in ["stdout", "stderr"] {
                    let trimmed = match keep_evidence {
                        true => failure[stream].as_str().map(evidence_tail),
                        false => None,
                    };
                    failure[stream] = json!(trimmed.unwrap_or_default());
                }
                if !keep_evidence {
                    for field in ["backtrace", "command", "reproduction"] {
                        failure[field] = Value::Null;
                    }
                    failure["artifacts"] = json!([]);
                }
            }
        }
    }
    value["stdout"] = json!("");
    value["stderr"] = json!("");
    value["detail"] = json!(detail);
    value["output_omitted"] = json!(true);
    value
}

/// The last stretch of one command stream, enough to hold its verdict.
const EVIDENCE_TAIL_BYTES: usize = 4096;

/// Takes the tail of `text` that fits the evidence budget, on a character
/// boundary, marking the cut so a reader knows the head exists in the session.
fn evidence_tail(text: &str) -> String {
    if text.len() <= EVIDENCE_TAIL_BYTES {
        return text.to_owned();
    }
    let mut start = text.len() - EVIDENCE_TAIL_BYTES;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    format!("[… earlier output in the session …]\n{}", &text[start..])
}

/// The three engines a Kira program can run on.
pub const BACKENDS: [&str; 3] = ["vm", "llvm", "hybrid"];

/// The devices a Kira program can be built for.
///
/// The same three spellings `--device` accepts, so a value that passes here
/// reaches the CLI unchanged.
pub const DEVICES: [&str; 3] = ["host", "wasm32", "wasm64"];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_named_tool_dispatches_and_nothing_else_does() {
        for name in [
            "kira_dev_build",
            "kira_dev_validate",
            "kira_dev_debug",
            "kira_dev_dump",
            "kira_dev_compare",
            "kira_dev_reproduce",
            "kira_dev_benchmark",
            "kira_dev_fuzz",
        ] {
            let listed = descriptors()
                .as_array()
                .expect("an array")
                .iter()
                .any(|tool| tool["name"] == json!(name));
            assert!(listed, "`{name}` must be advertised");
        }
        assert!(
            call("kira_dev_search", &json!({})).is_none(),
            "a tool outside the nine must not dispatch"
        );
    }

    /// The advertised set is exactly eight: the boundary is part of the design.
    #[test]
    fn the_surface_is_exactly_eight_tools() {
        assert_eq!(descriptors().as_array().expect("an array").len(), 8);
    }

    /// The retired tool is gone from the surface, not merely unlisted: a
    /// caller still asking for it must be told, rather than have the gate's
    /// tests run under a name that no longer means what it did.
    #[test]
    fn the_folded_in_test_tool_no_longer_dispatches() {
        assert!(call("kira_dev_test", &json!({ "suite": "all" })).is_none());
    }

    #[test]
    fn an_unknown_enum_value_is_refused_rather_than_defaulted() {
        let arguments = json!({ "backend": "llvm2" });
        let result = enum_field(&arguments, "backend", &BACKENDS, None);
        assert!(
            result.is_err(),
            "an unknown backend must not become a default"
        );
    }

    #[test]
    fn a_zero_timeout_is_refused() {
        assert!(timeout(&json!({ "timeout": 0 })).is_err());
    }
}
