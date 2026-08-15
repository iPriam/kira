//! JSON-RPC over stdio, framed the way MCP specifies.
//!
//! Messages are newline-delimited JSON objects on stdin and stdout. Nothing
//! else may be written to stdout: a stray `println!` is a parse error at the
//! client, which reads as the server having crashed. Everything diagnostic goes
//! to stderr.
//!
//! The toolchain has more than one MCP server — one answering compiler
//! questions, one owning debugger sessions — and a client cannot tell which
//! spoke a slightly different protocol. They share this crate so there is one
//! answer to `initialize`, one error shape, and one loop.

use std::io::{BufRead, Write};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// The MCP revision these servers speak.
pub const PROTOCOL_VERSION: &str = "2024-11-05";

/// One incoming JSON-RPC request.
#[derive(Debug, Deserialize)]
pub struct Request {
    /// Absent for a notification, which takes no reply.
    #[serde(default)]
    pub id: Option<Value>,
    /// The method being called.
    pub method: String,
    /// The method's parameters, `null` when it takes none.
    #[serde(default)]
    pub params: Value,
}

/// One outgoing JSON-RPC response.
#[derive(Debug, Serialize)]
pub struct Response {
    /// The JSON-RPC version, always `2.0`.
    pub jsonrpc: &'static str,
    /// The identifier of the request being answered.
    pub id: Value,
    /// The result, on success.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    /// The error, on failure.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ErrorObject>,
}

/// A JSON-RPC error body.
#[derive(Debug, Serialize)]
pub struct ErrorObject {
    /// The JSON-RPC error code.
    pub code: i32,
    /// What went wrong.
    pub message: String,
}

impl Response {
    /// A successful reply carrying `result`.
    #[must_use]
    pub fn ok(id: Value, result: Value) -> Self {
        Self {
            jsonrpc: "2.0",
            id,
            result: Some(result),
            error: None,
        }
    }

    /// A failed reply. `-32602` is JSON-RPC's invalid-params.
    #[must_use]
    pub fn invalid_params(id: Value, message: impl Into<String>) -> Self {
        Self {
            jsonrpc: "2.0",
            id,
            result: None,
            error: Some(ErrorObject {
                code: -32602,
                message: message.into(),
            }),
        }
    }

    /// A failed reply for a method this server does not implement.
    #[must_use]
    pub fn unknown_method(id: Value, method: &str) -> Self {
        Self {
            jsonrpc: "2.0",
            id,
            result: None,
            error: Some(ErrorObject {
                code: -32601,
                message: format!("unknown method `{method}`"),
            }),
        }
    }
}

/// The `initialize` result: who this server is and what it offers.
#[must_use]
pub fn initialize_result(name: &str, version: &str) -> Value {
    json!({
        "protocolVersion": PROTOCOL_VERSION,
        "capabilities": { "tools": {} },
        "serverInfo": { "name": name, "version": version },
    })
}

/// Wraps a tool's structured result in the MCP content envelope.
///
/// The structured value is carried twice on purpose: `structuredContent` for a
/// client that reads JSON, and the same JSON as text for one that only renders
/// content blocks. A tool that failed sets `isError`, so a caller cannot mistake
/// a captured failure for a successful call.
#[must_use]
pub fn tool_result(value: Value, is_error: bool) -> Value {
    let text = serde_json::to_string_pretty(&value).unwrap_or_else(|_| value.to_string());
    json!({
        "content": [{ "type": "text", "text": text }],
        "structuredContent": value,
        "isError": is_error,
    })
}

/// A server that answers one request at a time.
pub trait Server {
    /// The name reported to the client at initialization.
    fn name(&self) -> &str;

    /// The version reported to the client at initialization.
    fn version(&self) -> &str;

    /// Every tool this server offers, as MCP tool descriptors.
    fn descriptors(&mut self) -> Value;

    /// Runs one tool, returning its structured result and whether it failed.
    ///
    /// `None` means this server has no such tool, which is a protocol error
    /// rather than a failed call.
    fn call(&mut self, name: &str, arguments: &Value) -> Option<(Value, bool)>;
}

/// Reads requests from stdin and writes replies to stdout until input ends.
pub fn serve(server: &mut impl Server) {
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let Some(response) = handle(server, &line) else {
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
pub fn handle(server: &mut impl Server, line: &str) -> Option<Response> {
    let request: Request = match serde_json::from_str(line) {
        Ok(request) => request,
        Err(error) => {
            eprintln!("{}: cannot parse a request: {error}", server.name());
            return Some(Response::invalid_params(
                Value::Null,
                format!("cannot parse the request: {error}"),
            ));
        }
    };
    let id = request.id.clone()?;

    Some(match request.method.as_str() {
        "initialize" => Response::ok(id, initialize_result(server.name(), server.version())),
        "ping" => Response::ok(id, json!({})),
        "tools/list" => Response::ok(id, json!({ "tools": server.descriptors() })),
        "tools/call" => call_tool(server, id, &request.params),
        method => Response::unknown_method(id, method),
    })
}

/// Dispatches a `tools/call`.
fn call_tool(server: &mut impl Server, id: Value, params: &Value) -> Response {
    let Some(name) = params["name"].as_str() else {
        return Response::invalid_params(id, "`name` is required");
    };
    let arguments = match &params["arguments"] {
        Value::Null => json!({}),
        arguments => arguments.clone(),
    };
    match server.call(name, &arguments) {
        // A tool that ran and failed is a *successful call* carrying a failed
        // result: the caller asked what happened and this is what happened.
        // Only an unknown tool is a protocol-level error.
        Some((value, is_error)) => Response::ok(id, tool_result(value, is_error)),
        None => Response::invalid_params(id, format!("unknown tool `{name}`")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Echo;

    impl Server for Echo {
        fn name(&self) -> &str {
            "Echo"
        }

        fn version(&self) -> &str {
            "1.0.0"
        }

        fn descriptors(&mut self) -> Value {
            json!([{ "name": "echo", "description": "echoes", "inputSchema": { "type": "object" } }])
        }

        fn call(&mut self, name: &str, arguments: &Value) -> Option<(Value, bool)> {
            match name {
                "echo" => Some((arguments.clone(), false)),
                "fail" => Some((json!({ "success": false }), true)),
                _ => None,
            }
        }
    }

    fn request(method: &str, params: Value) -> String {
        json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params }).to_string()
    }

    #[test]
    fn initialize_announces_the_server_that_answered() {
        let response = handle(&mut Echo, &request("initialize", json!({}))).expect("a reply");
        let result = response.result.expect("a result");
        assert_eq!(result["serverInfo"]["name"], json!("Echo"));
        assert_eq!(result["protocolVersion"], json!(PROTOCOL_VERSION));
    }

    #[test]
    fn a_notification_is_not_answered() {
        let line = json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }).to_string();
        assert!(handle(&mut Echo, &line).is_none());
    }

    #[test]
    fn an_unknown_method_is_an_error_rather_than_an_empty_success() {
        let response = handle(&mut Echo, &request("resources/list", json!({}))).expect("a reply");
        assert_eq!(response.error.expect("an error").code, -32601);
    }

    #[test]
    fn an_unknown_tool_is_a_protocol_error() {
        let response = handle(
            &mut Echo,
            &request("tools/call", json!({ "name": "absent" })),
        )
        .expect("a reply");
        assert_eq!(response.error.expect("an error").code, -32602);
    }

    #[test]
    fn a_failing_tool_call_answers_with_an_error_result() {
        let response =
            handle(&mut Echo, &request("tools/call", json!({ "name": "fail" }))).expect("a reply");
        let result = response.result.expect("a result");
        assert_eq!(result["isError"], json!(true));
        assert_eq!(result["structuredContent"]["success"], json!(false));
    }

    #[test]
    fn unparseable_input_is_reported_rather_than_ignored() {
        let response = handle(&mut Echo, "{ not json").expect("a reply");
        assert_eq!(response.error.expect("an error").code, -32602);
    }

    #[test]
    fn the_text_block_repeats_the_structured_result() {
        let result = tool_result(json!({ "passed": 3 }), false);
        let text = result["content"][0]["text"].as_str().expect("text");
        let parsed: Value = serde_json::from_str(text).expect("the text block is json");
        assert_eq!(parsed, result["structuredContent"]);
    }
}
