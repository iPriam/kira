//! Machine code, loaded images, and the raw LLDB command interpreter.

use serde_json::{Value, json};

use super::{descriptor, required_string, session_field, session_property, uint_field};
use crate::registry::Sessions;
use crate::tools::inspect::request;

/// The code-inspection tools.
pub fn descriptors() -> Vec<Value> {
    vec![
        descriptor(
            "kira_lldb_disassemble",
            "Disassemble machine code at an address, or at the stopped frame when no \
             address is given.",
            json!({
                "session": session_property(),
                "address": {
                    "type": "string",
                    "description": "Where to start. Defaults to the stopped instruction pointer.",
                },
                "count": {
                    "type": "integer",
                    "description": "How many instructions to return. Defaults to 32.",
                },
                "offset": {
                    "type": "integer",
                    "description": "How many instructions to start before `address`.",
                },
            }),
            &[],
        ),
        descriptor(
            "kira_lldb_modules",
            "The executables and shared libraries loaded into the debugged process.",
            json!({ "session": session_property() }),
            &[],
        ),
        descriptor(
            "kira_lldb_command",
            "Run a raw LLDB command in the stopped process and return its output. The \
             escape hatch for anything the typed tools do not cover.",
            json!({
                "session": session_property(),
                "command": {
                    "type": "string",
                    "description": "The LLDB command, such as `image lookup --address $pc`.",
                },
            }),
            &["command"],
        ),
    ]
}

/// Disassembles machine code.
pub fn disassemble(sessions: &mut Sessions, arguments: &Value) -> Result<Value, String> {
    let count = uint_field(arguments, "count", 32)?;
    let offset = uint_field(arguments, "offset", 0)?;
    // The DAP field is signed and this tool walks backwards from the frame's
    // pointer, so the offset is negated — which only fits when it does.
    let offset = i64::try_from(offset)
        .map_err(|_| "`offset` is past what a disassembly window can address".to_owned())?;
    let address = arguments["address"].as_str().map(str::to_owned);
    let session = sessions.select(session_field(arguments))?;
    let frame_id = session.top_frame_id()?;
    let reference = match address {
        Some(address) => address,
        None => {
            let thread_id = session
                .client()
                .stopped_thread()
                .map_err(|error| error.to_string())?;
            let stack = request(
                session,
                "stackTrace",
                json!({ "threadId": thread_id, "startFrame": 0, "levels": 1 }),
            )?;
            stack["stackFrames"][0]["instructionPointerReference"]
                .as_str()
                .ok_or_else(|| "the stopped frame reports no instruction pointer".to_owned())?
                .to_owned()
        }
    };
    let reply = request(
        session,
        "disassemble",
        json!({
            "memoryReference": reference,
            "instructionOffset": -offset,
            "instructionCount": count,
            "resolveSymbols": true,
            "frameId": frame_id,
        }),
    )?;
    Ok(json!({ "address": reference, "instructions": reply["instructions"] }))
}

/// Lists the loaded images.
pub fn modules(sessions: &mut Sessions, arguments: &Value) -> Result<Value, String> {
    let session = sessions.select(session_field(arguments))?;
    let reply = request(session, "modules", json!({}))?;
    Ok(json!({ "modules": reply["modules"], "total": reply["totalModules"] }))
}

/// Runs a raw LLDB command.
///
/// LLDB's DAP frontend runs a command interpreter line when an evaluation is
/// made in the `repl` context, so this is the same interpreter `kira debug
/// --lldb` drives, reached without a second connection.
pub fn command(sessions: &mut Sessions, arguments: &Value) -> Result<Value, String> {
    let line = required_string(arguments, "command")?.to_owned();
    let session = sessions.select(session_field(arguments))?;
    let frame_id = session.top_frame_id().unwrap_or_default();
    let reply = request(
        session,
        "evaluate",
        json!({ "expression": line, "frameId": frame_id, "context": "repl" }),
    )?;
    Ok(json!({
        "command": line,
        "output": reply["result"],
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_raw_command_must_be_given() {
        let mut sessions = Sessions::default();
        let error = command(&mut sessions, &json!({})).expect_err("no command");
        assert!(error.contains("`command`"), "error was: {error}");
    }

    #[test]
    fn a_disassembly_count_must_be_a_number() {
        assert!(uint_field(&json!({ "count": "many" }), "count", 32).is_err());
        assert_eq!(uint_field(&json!({}), "count", 32), Ok(32));
    }
}
