//! The three questions only Kira can answer about a stopped program.
//!
//! A native debugger stopped in a Kira VM sees the interpreter: a probe frame,
//! the interpreter's own locals, and machine registers. None of that is the
//! program the caller wrote. These tools report the Kira side — which function
//! and instruction the VM is on, what its locals and operand stack hold, what
//! the Kira call stack is, and which source the whole thing came from.

use serde_json::{Value, json};

use super::{descriptor, session_field, session_property, uint_field};
use crate::registry::Sessions;

/// The Kira-side tools.
pub fn descriptors() -> Vec<Value> {
    vec![
        descriptor(
            "kira_lldb_state",
            "The decoded Kira state at a bytecode stop: the function and instruction \
             the VM is on, the instruction bytes, the frame's locals, the operand \
             stack, and the Kira call stack.",
            json!({ "session": session_property() }),
            &[],
        ),
        descriptor(
            "kira_lldb_functions",
            "The functions of the debugged program: their Kira names, identifiers, \
             native symbols where they have one, declaration lines, and whether each \
             runs as bytecode or machine code.",
            json!({
                "session": session_property(),
                "name": {
                    "type": "string",
                    "description": "Only functions whose name contains this text.",
                },
            }),
            &[],
        ),
        descriptor(
            "kira_lldb_source",
            "Kira source around a line, or around the function the VM is stopped in \
             when no line is given.",
            json!({
                "session": session_property(),
                "line": { "type": "integer", "description": "The line to centre on." },
                "context": {
                    "type": "integer",
                    "description": "How many lines to show on each side. Defaults to 8.",
                },
            }),
            &[],
        ),
    ]
}

/// Reports the decoded Kira stop.
pub fn state(sessions: &mut Sessions, arguments: &Value) -> Result<Value, String> {
    let session = sessions.select(session_field(arguments))?;
    let Some(probe) = session.target.probe.clone() else {
        return Err(format!(
            "the `{}` backend runs machine code, so there is no VM state; \
             use `kira_lldb_backtrace` and `kira_lldb_variables`",
            session.target.backend
        ));
    };
    match session.vm_stop()? {
        Some(stop) => Ok(json!({ "probe": probe.symbol, "kira": stop })),
        None => Err(format!(
            "the target is {} and has published no VM stop",
            session.state().label()
        )),
    }
}

/// Lists the program's functions.
pub fn functions(sessions: &mut Sessions, arguments: &Value) -> Result<Value, String> {
    let filter = arguments["name"].as_str().map(str::to_owned);
    let session = sessions.select(session_field(arguments))?;
    let functions = session
        .target
        .functions
        .iter()
        .filter(|function| {
            filter
                .as_deref()
                .is_none_or(|filter| function.name.contains(filter))
        })
        .collect::<Vec<_>>();
    Ok(json!({
        "module": session.target.module_name,
        "backend": session.target.backend,
        "source": session.target.source,
        "functions": functions,
    }))
}

/// Shows Kira source around a line or around the stopped function.
pub fn source(sessions: &mut Sessions, arguments: &Value) -> Result<Value, String> {
    let requested = uint_field(arguments, "line", 0)?;
    let context = u32::try_from(uint_field(arguments, "context", 8)?)
        .map_err(|_| "`context` is too large".to_owned())?;
    let session = sessions.select(session_field(arguments))?;
    let path = session.target.source.clone();
    let centre = match requested {
        0 => {
            let stop = session.vm_stop()?.ok_or_else(|| {
                "no line was given and the target has published no VM stop".to_owned()
            })?;
            session
                .target
                .function(&stop.function)
                .map(|function| function.line.max(1))
                .ok_or_else(|| format!("`{}` is not a function of this target", stop.function))?
        }
        line => u32::try_from(line).map_err(|_| "`line` is too large".to_owned())?,
    };
    let text = std::fs::read_to_string(&path)
        .map_err(|error| format!("cannot read `{}`: {error}", path.display()))?;
    let lines = text.lines().collect::<Vec<_>>();
    let (first, shown) = window(&lines, centre, context);
    Ok(json!({
        "path": path,
        "line": centre,
        "lines": shown
            .iter()
            .enumerate()
            .map(|(offset, text)| {
                let number = first + offset as u32;
                json!({ "number": number, "text": text, "current": number == centre })
            })
            .collect::<Vec<_>>(),
    }))
}

/// The lines shown around `centre`, and the number of the first of them.
///
/// Lines are one-based and the window is clamped to the file, so a stop near
/// the first or last line still shows as much context as exists rather than an
/// empty list.
fn window<'a>(lines: &[&'a str], centre: u32, context: u32) -> (u32, Vec<&'a str>) {
    if lines.is_empty() {
        return (centre, Vec::new());
    }
    let last = lines.len() as u32;
    let first = centre.saturating_sub(context).max(1).min(last);
    let end = centre.saturating_add(context).min(last);
    let shown = lines[first as usize - 1..end as usize].to_vec();
    (first, shown)
}

#[cfg(test)]
mod tests {
    use super::*;

    const LINES: [&str; 5] = ["one", "two", "three", "four", "five"];

    #[test]
    fn a_window_is_centred_on_the_line_that_was_asked_for() {
        let (first, shown) = window(&LINES, 3, 1);
        assert_eq!(first, 2);
        assert_eq!(shown, vec!["two", "three", "four"]);
    }

    /// A stop on the first line must still show what follows it.
    #[test]
    fn a_window_at_the_start_of_a_file_is_clamped_rather_than_emptied() {
        let (first, shown) = window(&LINES, 1, 3);
        assert_eq!(first, 1);
        assert_eq!(shown, vec!["one", "two", "three", "four"]);
    }

    #[test]
    fn a_window_at_the_end_of_a_file_stops_at_the_last_line() {
        let (first, shown) = window(&LINES, 5, 2);
        assert_eq!(first, 3);
        assert_eq!(shown, vec!["three", "four", "five"]);
    }

    /// A line past the end of the file must not panic on the slice.
    #[test]
    fn a_line_beyond_the_file_shows_the_end_of_it() {
        let (first, shown) = window(&LINES, 99, 2);
        assert_eq!(first, 5);
        assert_eq!(shown, vec!["five"]);
    }

    #[test]
    fn an_empty_file_shows_nothing_without_failing() {
        assert_eq!(window(&[], 1, 4), (1, Vec::new()));
    }

    #[test]
    fn no_context_shows_only_the_line_itself() {
        assert_eq!(window(&LINES, 2, 0), (2, vec!["two"]));
    }
}
