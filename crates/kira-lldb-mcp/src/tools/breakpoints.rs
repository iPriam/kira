//! Placing, listing, and removing breakpoints and watchpoints.

use std::path::PathBuf;

use kira_debug::DEFAULT_TIMEOUT;
use serde_json::{Value, json};

use super::{descriptor, enum_field, session_field, session_property, uint_field};
use crate::registry::Sessions;
use crate::session::{Placement, Session};

/// How a watchpoint may be triggered.
const ACCESSES: [&str; 3] = ["write", "read", "readWrite"];

/// The breakpoint tools.
pub fn descriptors() -> Vec<Value> {
    vec![
        descriptor(
            "kira_lldb_break_set",
            "Place a breakpoint. Give `function` for a Kira function — optionally with \
             `pc` for one bytecode instruction inside it — or `symbol` for a native \
             symbol, or `file` with `line` for a source line. `condition` is an LLDB \
             expression that must hold for the stop to happen.",
            json!({
                "session": session_property(),
                "function": {
                    "type": "string",
                    "description": "A Kira function name, its identifier, or its native symbol.",
                },
                "pc": {
                    "type": "integer",
                    "description": "The bytecode instruction index inside `function`. Defaults to 0.",
                },
                "symbol": {
                    "type": "string",
                    "description": "A native symbol to break on directly.",
                },
                "file": { "type": "string", "description": "A source file to break in." },
                "line": { "type": "integer", "description": "The line in `file` to break on." },
                "condition": {
                    "type": "string",
                    "description": "An LLDB expression that must be true for the stop to happen.",
                },
            }),
            &[],
        ),
        descriptor(
            "kira_lldb_break_list",
            "List the breakpoints a session holds and where each one resolved.",
            json!({ "session": session_property() }),
            &[],
        ),
        descriptor(
            "kira_lldb_break_delete",
            "Remove breakpoints by identifier, or all of them when none is named.",
            json!({
                "session": session_property(),
                "ids": {
                    "type": "array",
                    "items": { "type": "integer" },
                    "description": "The breakpoints to remove. All of them when absent.",
                },
            }),
            &[],
        ),
        descriptor(
            "kira_lldb_watch",
            "Watch an address or expression and stop when it is accessed.",
            json!({
                "session": session_property(),
                "expression": {
                    "type": "string",
                    "description": "The variable or address to watch.",
                },
                "access": {
                    "type": "string",
                    "enum": ACCESSES,
                    "description": "Which accesses stop the target. Defaults to write.",
                },
            }),
            &["expression"],
        ),
    ]
}

/// Places one breakpoint.
pub fn set(sessions: &mut Sessions, arguments: &Value) -> Result<Value, String> {
    let session = sessions.select(session_field(arguments))?;
    let condition = arguments["condition"].as_str().map(str::to_owned);
    let placement = placement(session, arguments)?;
    let breakpoint = session.add_breakpoint(placement, condition)?;
    Ok(json!({ "breakpoint": breakpoint }))
}

/// Lists a session's breakpoints.
pub fn list(sessions: &mut Sessions, arguments: &Value) -> Result<Value, String> {
    let session = sessions.select(session_field(arguments))?;
    Ok(json!({ "breakpoints": session.breakpoints() }))
}

/// Removes breakpoints.
pub fn delete(sessions: &mut Sessions, arguments: &Value) -> Result<Value, String> {
    let ids = breakpoint_ids(arguments)?;
    let session = sessions.select(session_field(arguments))?;
    let removed = session.remove_breakpoints(ids.as_deref())?;
    Ok(json!({ "removed": removed, "breakpoints": session.breakpoints() }))
}

/// Watches an expression.
pub fn watch(sessions: &mut Sessions, arguments: &Value) -> Result<Value, String> {
    let expression = super::required_string(arguments, "expression")?.to_owned();
    let access = enum_field(arguments, "access", &ACCESSES, "write")?.to_owned();
    let session = sessions.select(session_field(arguments))?;
    let frame_id = session.top_frame_id()?;
    let information = session
        .client()
        .request(
            "dataBreakpointInfo",
            json!({ "name": expression, "frameId": frame_id }),
            DEFAULT_TIMEOUT,
        )
        .map_err(|error| error.to_string())?;
    let reference = information["dataId"]
        .as_str()
        .ok_or_else(|| match information["description"].as_str() {
            Some(reason) => format!("`{expression}` cannot be watched: {reason}"),
            None => format!("`{expression}` cannot be watched here"),
        })?
        .to_owned();
    let reply = session
        .client()
        .request(
            "setDataBreakpoints",
            json!({ "breakpoints": [{ "dataId": reference, "accessType": access }] }),
            DEFAULT_TIMEOUT,
        )
        .map_err(|error| error.to_string())?;
    Ok(json!({
        "expression": expression,
        "access": access,
        "watchpoints": reply["breakpoints"],
    }))
}

/// Reads the placement one `kira_lldb_break_set` call describes.
fn placement(session: &Session, arguments: &Value) -> Result<Placement, String> {
    let function = arguments["function"].as_str();
    let symbol = arguments["symbol"].as_str();
    let file = arguments["file"].as_str();
    match (function, symbol, file) {
        (Some(function), None, None) => {
            let pc = uint_field(arguments, "pc", 0)?;
            let pc = u32::try_from(pc).map_err(|_| "`pc` is too large".to_owned())?;
            function_placement(session, function, pc)
        }
        (None, Some(symbol), None) => Ok(Placement::Function {
            symbol: symbol.to_owned(),
        }),
        (None, None, Some(file)) => {
            let line = uint_field(arguments, "line", 0)?;
            let line = u32::try_from(line).map_err(|_| "`line` is too large".to_owned())?;
            match line {
                0 => Err("`line` is required with `file`".to_owned()),
                line => Ok(Placement::Source {
                    path: PathBuf::from(file),
                    line,
                }),
            }
        }
        (None, None, None) => Err("give `function`, `symbol`, or `file` with `line`".to_owned()),
        _ => Err("give only one of `function`, `symbol`, or `file`".to_owned()),
    }
}

/// Resolves a Kira function to the placement its backend can stop at.
///
/// A function with a native body is broken on by symbol, because that is a
/// real address the debugger owns. One that only exists as bytecode is reached
/// through the VM probe instead, which is why the identifier is resolved here
/// rather than left as a name the adapter would fail to find.
fn function_placement(session: &Session, name: &str, pc: u32) -> Result<Placement, String> {
    let function = session.target.function(name).ok_or_else(|| {
        format!(
            "no function `{name}` in `{}`; `kira_lldb_functions` lists them",
            session.target.module_name
        )
    })?;
    match (&function.symbol, pc) {
        (Some(symbol), 0) => Ok(Placement::Function {
            symbol: symbol.clone(),
        }),
        (Some(_), _) if session.target.probe.is_none() => Err(format!(
            "`{name}` has a native body, so it stops at its entry rather than at instruction {pc}"
        )),
        _ => Ok(Placement::Kira {
            function: function.name.clone(),
            function_id: function.id,
            pc,
        }),
    }
}

/// Resolves a `function` or `function:instruction` spelling to a placement.
pub fn kira_placement(session: &Session, spelling: &str) -> Result<Placement, String> {
    let (name, pc) = split_spelling(spelling);
    function_placement(session, name, pc)
}

/// Splits `function:instruction` into its two halves.
///
/// A suffix that is not a number belongs to the name: `Grid.step` and
/// `step:entry` are both whole names, and taking the last colon as a separator
/// unconditionally would break on the first.
fn split_spelling(spelling: &str) -> (&str, u32) {
    match spelling.rsplit_once(':') {
        Some((name, pc)) => match pc.parse::<u32>() {
            Ok(pc) => (name, pc),
            Err(_) => (spelling, 0),
        },
        None => (spelling, 0),
    }
}

/// Reads the breakpoints a delete call names, `None` meaning all of them.
fn breakpoint_ids(arguments: &Value) -> Result<Option<Vec<u32>>, String> {
    const MESSAGE: &str = "`ids` must be an array of breakpoint identifiers";
    match &arguments["ids"] {
        Value::Null => Ok(None),
        Value::Array(items) => items
            .iter()
            .map(|item| {
                item.as_u64()
                    .and_then(|id| u32::try_from(id).ok())
                    .ok_or_else(|| MESSAGE.to_owned())
            })
            .collect::<Result<Vec<_>, _>>()
            .map(Some),
        _ => Err(MESSAGE.to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_breakpoint_needs_a_place_to_go() {
        let mut sessions = Sessions::default();
        assert!(set(&mut sessions, &json!({})).is_err());
    }

    #[test]
    fn identifiers_to_delete_must_be_numbers() {
        assert!(breakpoint_ids(&json!({ "ids": ["one"] })).is_err());
        assert!(breakpoint_ids(&json!({ "ids": 3 })).is_err());
        assert_eq!(
            breakpoint_ids(&json!({ "ids": [1, 4] })),
            Ok(Some(vec![1, 4]))
        );
    }

    /// Naming nothing removes everything, which is the documented behaviour
    /// and must not be confused with removing an empty list.
    #[test]
    fn naming_no_identifier_means_every_breakpoint() {
        assert_eq!(breakpoint_ids(&json!({})), Ok(None));
        assert_eq!(breakpoint_ids(&json!({ "ids": [] })), Ok(Some(Vec::new())));
    }

    #[test]
    fn a_bare_function_name_means_its_first_instruction() {
        assert_eq!(split_spelling("discountAmount"), ("discountAmount", 0));
    }

    #[test]
    fn a_trailing_instruction_index_is_read_off_the_spelling() {
        assert_eq!(split_spelling("discountAmount:3"), ("discountAmount", 3));
    }

    /// A qualified Kira name keeps its colon-free reading: `Grid.step` has no
    /// instruction index, and neither does a name whose suffix is not a number.
    #[test]
    fn a_name_whose_suffix_is_not_a_number_keeps_the_whole_spelling() {
        assert_eq!(split_spelling("Grid.step"), ("Grid.step", 0));
        assert_eq!(split_spelling("step:entry"), ("step:entry", 0));
    }
}
