//! JSON-RPC over stdio, framed the way MCP specifies.
//!
//! Messages are newline-delimited JSON objects on stdin and stdout. Nothing
//! else may be written to stdout: a stray `println!` is a parse error at the
//! client, which reads as the server having crashed. Everything diagnostic goes
//! to stderr.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// The MCP revision this server speaks.
pub const PROTOCOL_VERSION: &str = "2024-11-05";

/// One incoming JSON-RPC request.
#[derive(Debug, Deserialize)]
pub struct Request {
    /// Absent for a notification, which takes no reply.
    #[serde(default)]
    pub id: Option<Value>,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

/// One outgoing JSON-RPC response.
#[derive(Debug, Serialize)]
pub struct Response {
    pub jsonrpc: &'static str,
    pub id: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ErrorObject>,
}

/// A JSON-RPC error body.
#[derive(Debug, Serialize)]
pub struct ErrorObject {
    pub code: i32,
    pub message: String,
}

impl Response {
    /// A successful reply carrying `result`.
    pub fn ok(id: Value, result: Value) -> Response {
        Response {
            jsonrpc: "2.0",
            id,
            result: Some(result),
            error: None,
        }
    }

    /// A failed reply. `-32602` is JSON-RPC's invalid-params.
    pub fn invalid_params(id: Value, message: impl Into<String>) -> Response {
        Response {
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
    pub fn unknown_method(id: Value, method: &str) -> Response {
        Response {
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
pub fn initialize_result() -> Value {
    json!({
        "protocolVersion": PROTOCOL_VERSION,
        "capabilities": { "tools": {} },
        "serverInfo": {
            "name": "Kira Toolchain Developer",
            "version": env!("CARGO_PKG_VERSION"),
        },
    })
}

/// Wraps a tool's structured result in the MCP content envelope.
///
/// The structured value is carried twice on purpose: `structuredContent` for a
/// client that reads JSON, and the same JSON as text for one that only renders
/// content blocks. A tool that failed sets `isError`, so a caller cannot mistake
/// a captured failure for a successful call.
pub fn tool_result(value: Value, is_error: bool) -> Value {
    let text = serde_json::to_string_pretty(&value).unwrap_or_else(|_| value.to_string());
    json!({
        "content": [{ "type": "text", "text": text }],
        "structuredContent": value,
        "isError": is_error,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failed_tool_call_is_marked_as_an_error() {
        let result = tool_result(json!({ "success": false }), true);
        assert_eq!(result["isError"], json!(true));
        assert_eq!(result["structuredContent"]["success"], json!(false));
    }

    /// The text block carries the same JSON, so a client that renders only
    /// content still sees every field.
    #[test]
    fn the_text_block_repeats_the_structured_result() {
        let result = tool_result(json!({ "passed": 3 }), false);
        let text = result["content"][0]["text"].as_str().expect("text");
        let parsed: Value = serde_json::from_str(text).expect("the text block is json");
        assert_eq!(parsed, result["structuredContent"]);
    }
}
