//! Reading the stopped process: frames, variables, expressions, registers,
//! memory, and threads.

use kira_debug::DEFAULT_TIMEOUT;
use serde_json::{Value, json};

use super::{
    descriptor, required_string, session_field, session_property, string_field, uint_field,
};
use crate::registry::Sessions;
use crate::session::Session;

/// The inspection tools.
pub fn descriptors() -> Vec<Value> {
    vec![
        descriptor(
            "kira_lldb_backtrace",
            "The native call stack of a stopped thread. For a bytecode program this is \
             the interpreter's own stack; the Kira call stack is in `kira_lldb_state`.",
            json!({
                "session": session_property(),
                "thread": { "type": "integer", "description": "The thread to walk. Defaults to the stopped one." },
                "levels": { "type": "integer", "description": "How many frames to return. Defaults to 32." },
            }),
            &[],
        ),
        descriptor(
            "kira_lldb_variables",
            "The variables visible in a stopped frame, by scope.",
            json!({
                "session": session_property(),
                "frame": { "type": "integer", "description": "The frame identifier. Defaults to the innermost." },
                "scope": { "type": "string", "description": "Only the scope with this name, such as Locals." },
            }),
            &[],
        ),
        descriptor(
            "kira_lldb_evaluate",
            "Evaluate an expression in a stopped frame and return its value.",
            json!({
                "session": session_property(),
                "expression": { "type": "string", "description": "The expression to evaluate." },
                "frame": { "type": "integer", "description": "The frame to evaluate in." },
                "context": {
                    "type": "string",
                    "description": "The DAP evaluation context: repl, watch, or hover. Defaults to repl.",
                },
            }),
            &["expression"],
        ),
        descriptor(
            "kira_lldb_registers",
            "The CPU registers of a stopped frame.",
            json!({
                "session": session_property(),
                "frame": { "type": "integer", "description": "The frame to read. Defaults to the innermost." },
            }),
            &[],
        ),
        descriptor(
            "kira_lldb_read_memory",
            "Read bytes from the stopped process.",
            json!({
                "session": session_property(),
                "address": {
                    "type": "string",
                    "description": "An address, or an expression that yields one such as `&KIRA_VM_DEBUG_TEXT`.",
                },
                "count": { "type": "integer", "description": "How many bytes to read. Defaults to 64." },
                "format": {
                    "type": "string",
                    "enum": ["hex", "text", "base64"],
                    "description": "How to render the bytes. Defaults to hex.",
                },
            }),
            &["address"],
        ),
        descriptor(
            "kira_lldb_write_memory",
            "Write bytes into the stopped process.",
            json!({
                "session": session_property(),
                "address": { "type": "string", "description": "The address to write to." },
                "bytes": { "type": "string", "description": "The bytes to write, as hexadecimal." },
            }),
            &["address", "bytes"],
        ),
        descriptor(
            "kira_lldb_threads",
            "Every thread in the debugged process.",
            json!({ "session": session_property() }),
            &[],
        ),
    ]
}

/// Walks a thread's native stack.
pub fn backtrace(sessions: &mut Sessions, arguments: &Value) -> Result<Value, String> {
    let levels = uint_field(arguments, "levels", 32)?;
    let thread = uint_field(arguments, "thread", 0)?;
    let session = sessions.select(session_field(arguments))?;
    let thread_id = match thread {
        0 => session
            .client()
            .stopped_thread()
            .map_err(|error| error.to_string())?,
        thread => thread as i64,
    };
    let stack = request(
        session,
        "stackTrace",
        json!({ "threadId": thread_id, "startFrame": 0, "levels": levels }),
    )?;
    Ok(json!({
        "thread": thread_id,
        "frames": stack["stackFrames"],
        "total": stack["totalFrames"],
    }))
}

/// Reads the variables of a stopped frame.
pub fn variables(sessions: &mut Sessions, arguments: &Value) -> Result<Value, String> {
    let wanted = arguments["scope"].as_str().map(str::to_owned);
    let frame = uint_field(arguments, "frame", 0)?;
    let session = sessions.select(session_field(arguments))?;
    let frame_id = match frame {
        0 => session.top_frame_id()?,
        frame => frame as i64,
    };
    let scopes = request(session, "scopes", json!({ "frameId": frame_id }))?;
    let mut reported = Vec::new();
    for scope in scopes["scopes"].as_array().unwrap_or(&Vec::new()) {
        let name = scope["name"].as_str().unwrap_or_default().to_owned();
        if wanted.as_deref().is_some_and(|wanted| wanted != name) {
            continue;
        }
        let Some(reference) = scope["variablesReference"].as_i64().filter(|id| *id > 0) else {
            continue;
        };
        let values = request(
            session,
            "variables",
            json!({ "variablesReference": reference }),
        )?;
        reported.push(json!({ "scope": name, "variables": values["variables"] }));
    }
    Ok(json!({ "frame": frame_id, "scopes": reported }))
}

/// Evaluates an expression in a stopped frame.
pub fn evaluate(sessions: &mut Sessions, arguments: &Value) -> Result<Value, String> {
    let expression = required_string(arguments, "expression")?.to_owned();
    let context = string_field(arguments, "context", "repl").to_owned();
    let frame = uint_field(arguments, "frame", 0)?;
    let session = sessions.select(session_field(arguments))?;
    let frame_id = match frame {
        0 => session.top_frame_id()?,
        frame => frame as i64,
    };
    let reply = request(
        session,
        "evaluate",
        json!({ "expression": expression, "frameId": frame_id, "context": context }),
    )?;
    Ok(json!({
        "expression": expression,
        "result": reply["result"],
        "type": reply["type"],
        "memory_reference": reply["memoryReference"],
    }))
}

/// Reads the registers of a stopped frame.
///
/// LLDB reports registers as a variable scope, so they are read the same way
/// locals are rather than through a command whose text would need parsing.
pub fn registers(sessions: &mut Sessions, arguments: &Value) -> Result<Value, String> {
    let frame = uint_field(arguments, "frame", 0)?;
    let session = sessions.select(session_field(arguments))?;
    let frame_id = match frame {
        0 => session.top_frame_id()?,
        frame => frame as i64,
    };
    let scopes = request(session, "scopes", json!({ "frameId": frame_id }))?;
    let register_scope = scopes["scopes"]
        .as_array()
        .and_then(|scopes| {
            scopes.iter().find(|scope| {
                scope["presentationHint"] == json!("registers")
                    || scope["name"]
                        .as_str()
                        .is_some_and(|name| name == "Registers")
            })
        })
        .cloned()
        .ok_or_else(|| "the stopped frame reports no registers".to_owned())?;
    let reference = register_scope["variablesReference"]
        .as_i64()
        .ok_or_else(|| "the register scope has no contents".to_owned())?;
    let groups = request(
        session,
        "variables",
        json!({ "variablesReference": reference }),
    )?;
    let mut reported = Vec::new();
    for group in groups["variables"].as_array().unwrap_or(&Vec::new()) {
        match group["variablesReference"].as_i64().filter(|id| *id > 0) {
            Some(reference) => {
                let values = request(
                    session,
                    "variables",
                    json!({ "variablesReference": reference }),
                )?;
                reported.push(json!({
                    "group": group["name"],
                    "registers": values["variables"],
                }));
            }
            None => reported.push(json!({
                "group": group["name"],
                "registers": [group.clone()],
            })),
        }
    }
    Ok(json!({ "frame": frame_id, "groups": reported }))
}

/// Reads bytes out of the stopped process.
pub fn read_memory(sessions: &mut Sessions, arguments: &Value) -> Result<Value, String> {
    let address = required_string(arguments, "address")?.to_owned();
    let count = uint_field(arguments, "count", 64)?;
    let format =
        super::enum_field(arguments, "format", &["hex", "text", "base64"], "hex")?.to_owned();
    let session = sessions.select(session_field(arguments))?;
    let reference = resolve_address(session, &address)?;
    let reply = request(
        session,
        "readMemory",
        json!({ "memoryReference": reference, "offset": 0, "count": count }),
    )?;
    let data = reply["data"].as_str().unwrap_or_default();
    let bytes = kira_debug::decode_base64(data);
    Ok(json!({
        "address": reference,
        "count": bytes.len(),
        "data": render(&bytes, &format, data),
        "format": format,
    }))
}

/// Writes bytes into the stopped process.
///
/// Through the command interpreter rather than the protocol's `writeMemory`
/// request: the LLDB the Swift toolchains ship exits on that request, and a
/// tool that ends the session it was asked to modify is worse than no tool.
pub fn write_memory(sessions: &mut Sessions, arguments: &Value) -> Result<Value, String> {
    let address = required_string(arguments, "address")?.to_owned();
    // The address is interpolated into an LLDB command line, so it must be
    // exactly one hexadecimal word: anything else — spaces, flags, extra
    // tokens — would be interpreted as command syntax rather than refused.
    let address = address.trim();
    let digits = address
        .strip_prefix("0x")
        .or_else(|| address.strip_prefix("0X"))
        .unwrap_or(address);
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("`address` must be a single hexadecimal address".to_owned());
    }
    let bytes = parse_hex(required_string(arguments, "bytes")?)?;
    let session = sessions.select(session_field(arguments))?;
    let frame_id = session.top_frame_id()?;
    let values = write_values(&bytes);
    let reply = request(
        session,
        "evaluate",
        json!({
            "expression": format!("memory write -s 1 0x{digits} {values}"),
            "frameId": frame_id,
            "context": "repl",
        }),
    )?;
    Ok(json!({
        "address": format!("0x{digits}"),
        "written": bytes.len(),
        "output": reply["result"],
    }))
}

/// Lists the process's threads.
pub fn threads(sessions: &mut Sessions, arguments: &Value) -> Result<Value, String> {
    let session = sessions.select(session_field(arguments))?;
    let reply = request(session, "threads", json!({}))?;
    Ok(json!({ "threads": reply["threads"] }))
}

/// Sends one request to the session's adapter.
pub fn request(session: &mut Session, command: &str, arguments: Value) -> Result<Value, String> {
    session
        .client()
        .request(command, arguments, DEFAULT_TIMEOUT)
        .map_err(|error| error.to_string())
}

/// Turns an address or an expression yielding one into a memory reference.
///
/// A caller who has an address uses it; one who has a name — `&KIRA_VM_DEBUG_TEXT`,
/// or a local — should not have to evaluate it first only to paste the result
/// back into the next call.
fn resolve_address(session: &mut Session, address: &str) -> Result<String, String> {
    if address.starts_with("0x") || address.chars().all(|character| character.is_ascii_digit()) {
        return Ok(address.to_owned());
    }
    let frame_id = session.top_frame_id()?;
    let reply = request(
        session,
        "evaluate",
        json!({ "expression": address, "frameId": frame_id, "context": "repl" }),
    )?;
    if let Some(reference) = reply["memoryReference"].as_str() {
        return Ok(reference.to_owned());
    }
    reply["result"]
        .as_str()
        .and_then(kira_debug::parse_address)
        .ok_or_else(|| format!("`{address}` did not evaluate to an address"))
}

/// Renders read bytes in the requested form.
fn render(bytes: &[u8], format: &str, base64: &str) -> Value {
    match format {
        "text" => {
            let length = bytes
                .iter()
                .position(|byte| *byte == 0)
                .unwrap_or(bytes.len());
            json!(String::from_utf8_lossy(&bytes[..length]))
        }
        "base64" => json!(base64),
        _ => json!(
            bytes
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<Vec<_>>()
                .join(" ")
        ),
    }
}

/// Parses hexadecimal bytes, with or without separators.
fn parse_hex(text: &str) -> Result<Vec<u8>, String> {
    let digits = text
        .chars()
        .filter(|character| !character.is_whitespace() && *character != ',')
        .collect::<String>();
    let digits = digits.strip_prefix("0x").unwrap_or(&digits);
    if digits.is_empty() || digits.len() % 2 != 0 {
        return Err("`bytes` must be an even number of hexadecimal digits".to_owned());
    }
    (0..digits.len())
        .step_by(2)
        .map(|index| {
            u8::from_str_radix(&digits[index..index + 2], 16)
                .map_err(|_| "`bytes` must be hexadecimal".to_owned())
        })
        .collect()
}

/// The `memory write` values one byte sequence becomes.
fn write_values(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|byte| format!("0x{byte:02x}"))
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hexadecimal_bytes_are_read_with_or_without_separators() {
        assert_eq!(parse_hex("deadbeef"), Ok(vec![0xde, 0xad, 0xbe, 0xef]));
        assert_eq!(parse_hex("de ad be ef"), Ok(vec![0xde, 0xad, 0xbe, 0xef]));
        assert_eq!(parse_hex("0xdead"), Ok(vec![0xde, 0xad]));
    }

    /// Half a byte is not a byte: writing it would put something in memory
    /// that the caller did not spell.
    #[test]
    fn an_odd_number_of_digits_is_refused() {
        assert!(parse_hex("abc").is_err());
        assert!(parse_hex("").is_err());
        assert!(parse_hex("zz").is_err());
    }

    /// Every byte is written back exactly, including the ones whose hexadecimal
    /// is a single digit: `0x0` and `0x00` are the same byte, but a value list
    /// that dropped the padding would be read as a different length.
    #[test]
    fn written_bytes_become_one_padded_value_each() {
        assert_eq!(write_values(&[0x00, 0x0f, 0xff]), "0x00 0x0f 0xff");
        assert_eq!(write_values(&[0x4b]), "0x4b");
        assert_eq!(write_values(&[]), "");
    }

    #[test]
    fn a_written_sequence_round_trips_through_the_hexadecimal_it_was_given() {
        let bytes = parse_hex("4b697261").expect("hexadecimal");
        assert_eq!(bytes, b"Kira");
        assert_eq!(write_values(&bytes), "0x4b 0x69 0x72 0x61");
    }

    #[test]
    fn read_bytes_render_in_the_form_that_was_asked_for() {
        let bytes = [0x4b, 0x69, 0x72, 0x61, 0x00, 0x7f];
        assert_eq!(render(&bytes, "hex", ""), json!("4b 69 72 61 00 7f"));
        assert_eq!(render(&bytes, "text", ""), json!("Kira"));
        assert_eq!(render(&bytes, "base64", "S2lyYQ=="), json!("S2lyYQ=="));
    }

    #[test]
    fn evaluating_needs_an_expression() {
        let mut sessions = Sessions::default();
        let error = evaluate(&mut sessions, &json!({})).expect_err("no expression");
        assert!(error.contains("`expression`"), "error was: {error}");
    }

    #[test]
    fn writing_needs_both_an_address_and_bytes() {
        let mut sessions = Sessions::default();
        assert!(write_memory(&mut sessions, &json!({ "address": "0x10" })).is_err());
        assert!(write_memory(&mut sessions, &json!({ "bytes": "ff" })).is_err());
    }
}
