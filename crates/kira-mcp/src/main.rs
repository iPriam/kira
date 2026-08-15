//! The Kira Toolchain Developer MCP server.
//!
//! An agent working on this repository needs answers a shell cannot give
//! reliably: did the build reach the backend or stop in the type checker, which
//! test failed and with what panic, where do the VM and the native backend stop
//! agreeing. Those are the eight tools here. Searching, reading and writing
//! files are not, because the client already does them, and a second worse copy
//! of them inside this server would be chosen sometimes.
//!
//! Debugging is not here either: a debug session outlives one call, and the
//! server that owns those sessions is `kira-lldb-mcp`.

use kira_mcp_protocol::{Server, serve};
use serde_json::Value;

mod exec;
mod schema;
mod session;
mod tools;

/// The toolchain developer server.
struct Toolchain;

impl Server for Toolchain {
    fn name(&self) -> &str {
        "Kira Toolchain Developer"
    }

    fn version(&self) -> &str {
        env!("CARGO_PKG_VERSION")
    }

    fn descriptors(&mut self) -> Value {
        tools::descriptors()
    }

    fn call(&mut self, name: &str, arguments: &Value) -> Option<(Value, bool)> {
        tools::call(name, arguments)
    }
}

fn main() {
    serve(&mut Toolchain);
}

#[cfg(test)]
mod tests {
    use super::*;
    use kira_mcp_protocol::{PROTOCOL_VERSION, handle};
    use serde_json::json;

    fn request(method: &str, params: Value) -> String {
        json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params }).to_string()
    }

    #[test]
    fn initialize_announces_the_server_and_its_tools() {
        let response = handle(&mut Toolchain, &request("initialize", json!({}))).expect("a reply");
        let result = response.result.expect("a result");
        assert_eq!(
            result["serverInfo"]["name"],
            json!("Kira Toolchain Developer")
        );
        assert_eq!(result["protocolVersion"], json!(PROTOCOL_VERSION));
    }

    #[test]
    fn the_tool_list_carries_every_tool_with_a_schema() {
        let response = handle(&mut Toolchain, &request("tools/list", json!({}))).expect("a reply");
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
            let response = handle(
                &mut Toolchain,
                &request("tools/call", json!({ "name": name, "arguments": {} })),
            )
            .expect("a reply");
            assert!(response.error.is_some(), "`{name}` must not be callable");
        }
    }

    /// Debugging belongs to the session server, not to this one.
    #[test]
    fn the_debug_session_tools_belong_to_the_other_server() {
        for name in ["kira_lldb_launch", "kira_lldb_continue", "kira_lldb_frames"] {
            let response = handle(
                &mut Toolchain,
                &request("tools/call", json!({ "name": name, "arguments": {} })),
            )
            .expect("a reply");
            assert!(response.error.is_some(), "`{name}` must not be callable");
        }
    }

    /// A tool that ran and failed still answers, marked as an error result.
    #[test]
    fn a_failing_tool_call_answers_with_an_error_result() {
        let response = handle(
            &mut Toolchain,
            &request(
                "tools/call",
                json!({ "name": "kira_dev_validate", "arguments": { "suite": "not-a-suite" } }),
            ),
        )
        .expect("a reply");
        let result = response.result.expect("a result");
        assert_eq!(result["isError"], json!(true));
        assert_eq!(result["structuredContent"]["success"], json!(false));
    }
}
