//! The Kira Toolchain Developer MCP server.
//!
//! An agent working on this repository needs answers a shell cannot give
//! reliably: did the build reach the backend or stop in the type checker, which
//! test failed and with what panic, where do the VM and the native backend stop
//! agreeing. Those are the eight tools here. Searching, reading and writing
//! files are not, because the client already does them, and a second worse copy
//! of them inside this server would be chosen sometimes.

use std::io::{BufRead, Write};

use serde_json::{Value, json};

mod exec;
mod protocol;
mod schema;
mod session;
mod tools;

use protocol::{Request, Response};

fn main() {
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let Some(response) = handle(&line) else {
            continue;
        };
        let Ok(rendered) = serde_json::to_string(&response) else {
            continue;
        };
        // A write that fails means the client is gone; there is nothing left to
        // serve and nowhere to report it.
        if writeln!(stdout, "{rendered}").is_err() || stdout.flush().is_err() {
            break;
        }
    }
}

/// Handles one line of input, returning the reply when one is owed.
///
/// A notification carries no `id` and takes no reply: answering one is a
/// protocol error at the client, so `None` is the correct output rather than an
/// empty response.
fn handle(line: &str) -> Option<Response> {
    let request: Request = match serde_json::from_str(line) {
        Ok(request) => request,
        Err(error) => {
            eprintln!("kira-mcp: cannot parse a request: {error}");
            return Some(Response::invalid_params(
                Value::Null,
                format!("cannot parse the request: {error}"),
            ));
        }
    };
    let id = request.id.clone()?;

    Some(match request.method.as_str() {
        "initialize" => Response::ok(id, protocol::initialize_result()),
        "ping" => Response::ok(id, json!({})),
        "tools/list" => Response::ok(id, json!({ "tools": tools::descriptors() })),
        "tools/call" => call_tool(id, &request.params),
        method => Response::unknown_method(id, method),
    })
}

/// Dispatches a `tools/call`.
fn call_tool(id: Value, params: &Value) -> Response {
    let Some(name) = params["name"].as_str() else {
        return Response::invalid_params(id, "`name` is required");
    };
    let arguments = match &params["arguments"] {
        Value::Null => json!({}),
        arguments => arguments.clone(),
    };
    match tools::call(name, &arguments) {
        // A tool that ran and failed is a *successful call* carrying a failed
        // result: the caller asked what happened and this is what happened.
        // Only an unknown tool is a protocol-level error.
        Some((value, is_error)) => Response::ok(id, protocol::tool_result(value, is_error)),
        None => Response::invalid_params(id, format!("unknown tool `{name}`")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(method: &str, params: Value) -> String {
        json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params }).to_string()
    }

    #[test]
    fn initialize_announces_the_server_and_its_tools() {
        let response = handle(&request("initialize", json!({}))).expect("a reply");
        let result = response.result.expect("a result");
        assert_eq!(
            result["serverInfo"]["name"],
            json!("Kira Toolchain Developer")
        );
        assert_eq!(result["protocolVersion"], json!(protocol::PROTOCOL_VERSION));
    }

    #[test]
    fn the_tool_list_carries_every_tool_with_a_schema() {
        let response = handle(&request("tools/list", json!({}))).expect("a reply");
        let tools = response.result.expect("a result")["tools"]
            .as_array()
            .expect("an array")
            .clone();
        assert_eq!(tools.len(), 8);
        for tool in &tools {
            assert!(tool["name"].is_string(), "every tool is named");
            assert!(tool["description"].is_string(), "every tool is described");
            assert_eq!(tool["inputSchema"]["type"], json!("object"));
        }
    }

    /// The tools this server deliberately does not offer stay unoffered.
    #[test]
    fn the_client_owned_tools_are_absent() {
        for name in [
            "kira_dev_search",
            "kira_dev_read_file",
            "kira_dev_write_file",
            "kira_dev_shell",
            "kira_dev_logs",
            "kira_dev_status",
            "kira_dev_session",
            "kira_dev_capabilities",
            "kira_dev_find_tests",
            "kira_dev_resolve_symbol",
            "kira_dev_trace_construct",
        ] {
            let response = handle(&request(
                "tools/call",
                json!({ "name": name, "arguments": {} }),
            ))
            .expect("a reply");
            assert!(response.error.is_some(), "`{name}` must not be callable");
        }
    }

    /// A notification takes no reply, whatever it asked for.
    #[test]
    fn a_notification_is_not_answered() {
        let line = json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }).to_string();
        assert!(handle(&line).is_none());
    }

    #[test]
    fn an_unknown_method_is_an_error_rather_than_an_empty_success() {
        let response = handle(&request("resources/list", json!({}))).expect("a reply");
        assert_eq!(response.error.expect("an error").code, -32601);
    }

    /// A tool that ran and failed still answers, marked as an error result.
    #[test]
    fn a_failing_tool_call_answers_with_an_error_result() {
        let response = handle(&request(
            "tools/call",
            json!({ "name": "kira_dev_validate", "arguments": { "suite": "not-a-suite" } }),
        ))
        .expect("a reply");
        let result = response.result.expect("a result");
        assert_eq!(result["isError"], json!(true));
        assert_eq!(result["structuredContent"]["success"], json!(false));
    }

    #[test]
    fn unparseable_input_is_reported_rather_than_ignored() {
        let response = handle("{ not json").expect("a reply");
        assert_eq!(response.error.expect("an error").code, -32602);
    }
}
