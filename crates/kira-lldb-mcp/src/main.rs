//! The Kira LLDB MCP server.
//!
//! A debug session is a process that has to stay alive between questions. A
//! command that builds, stops once, prints a transcript and exits cannot be
//! asked "and what are the locals now" — so this server holds the sessions,
//! and every tool names one it already started.
//!
//! It debugs Kira programs on all three backends. On LLVM that is ordinary
//! native debugging over DWARF. On the VM and hybrid backends there is no
//! machine code to step, so the session drives the interpreter's own probe and
//! reports the decoded Kira function, instruction, locals, operand stack, and
//! call stack that a native debugger alone cannot see.

use kira_mcp_protocol::{Server, serve};
use serde_json::Value;

mod build;
mod registry;
mod report;
mod session;
mod tools;

use registry::Sessions;

/// The debug-session server and everything it is holding open.
#[derive(Default)]
struct Debugger {
    sessions: Sessions,
}

impl Server for Debugger {
    fn name(&self) -> &str {
        "Kira LLDB"
    }

    fn version(&self) -> &str {
        env!("CARGO_PKG_VERSION")
    }

    fn descriptors(&mut self) -> Value {
        tools::descriptors()
    }

    fn call(&mut self, name: &str, arguments: &Value) -> Option<(Value, bool)> {
        tools::call(&mut self.sessions, name, arguments)
    }
}

fn main() {
    let mut debugger = Debugger::default();
    serve(&mut debugger);
    // The client is gone. Every session still open owns a debugger process and
    // build artifacts on disk, and nothing else will ever close them.
    debugger.sessions.close_all();
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
    fn initialize_announces_the_debug_session_server() {
        let mut debugger = Debugger::default();
        let response = handle(&mut debugger, &request("initialize", json!({}))).expect("a reply");
        let result = response.result.expect("a result");
        assert_eq!(result["serverInfo"]["name"], json!("Kira LLDB"));
        assert_eq!(result["protocolVersion"], json!(PROTOCOL_VERSION));
    }

    #[test]
    fn the_tool_list_carries_every_tool_with_a_schema() {
        let mut debugger = Debugger::default();
        let response = handle(&mut debugger, &request("tools/list", json!({}))).expect("a reply");
        let tools = response.result.expect("a result")["tools"]
            .as_array()
            .expect("an array")
            .clone();
        assert_eq!(tools.len(), 25);
        for tool in &tools {
            assert!(tool["name"].is_string(), "every tool is named");
            assert!(tool["description"].is_string(), "every tool is described");
            assert_eq!(tool["inputSchema"]["type"], json!("object"));
        }
    }

    /// The compiler questions belong to the toolchain server, not to this one.
    #[test]
    fn the_toolchain_tools_are_not_offered_here() {
        let mut debugger = Debugger::default();
        for name in ["kira_dev_build", "kira_dev_validate", "kira_dev_dump"] {
            let response = handle(
                &mut debugger,
                &request("tools/call", json!({ "name": name, "arguments": {} })),
            )
            .expect("a reply");
            assert!(response.error.is_some(), "`{name}` must not be callable");
        }
    }

    /// A tool called with no session open answers, rather than taking the
    /// server down and leaving the client without a debugger at all.
    #[test]
    fn a_tool_called_before_any_session_exists_answers_with_a_failure() {
        let mut debugger = Debugger::default();
        let response = handle(
            &mut debugger,
            &request("tools/call", json!({ "name": "kira_lldb_state" })),
        )
        .expect("a reply");
        let result = response.result.expect("a result");
        assert_eq!(result["isError"], json!(true));
        assert_eq!(result["structuredContent"]["success"], json!(false));
    }
}
