//! What a caller is told after every tool that changes where a target is.
//!
//! One shape for all of them. A caller that resumed, stepped, or only asked
//! for status reads the same fields, so the answer to "where am I now" never
//! depends on which tool got them there.

use kira_debug::{DEFAULT_TIMEOUT, TargetState};
use serde_json::{Value, json};

use crate::session::Session;

/// Describes where a session is, including the decoded Kira stop when there
/// is one.
pub fn stop_report(session: &mut Session) -> Value {
    let state = session.state().clone();
    let mut report = json!({
        "session": session.id,
        "state": state.label(),
        "backend": session.target.backend,
    });
    match &state {
        TargetState::Stopped(stop) => {
            report["stop"] = json!({
                "reason": stop.reason,
                "thread": stop.thread_id,
                "description": stop.description,
                "hit_breakpoints": stop.hit_breakpoints,
            });
            report["frame"] = top_frame(session);
            if let Ok(Some(kira)) = session.vm_stop() {
                report["source_line"] = source_line(&session.target, &kira);
                report["kira"] = serde_json::to_value(kira).unwrap_or(Value::Null);
            }
        }
        TargetState::Exited(code) => report["exit_code"] = json!(code),
        _ => {}
    }
    let output = session.client().take_output();
    if !output.is_empty() {
        report["output"] = json!(output);
    }
    report
}

/// The innermost native frame, as far as the adapter will describe it.
fn top_frame(session: &mut Session) -> Value {
    let Ok(thread_id) = session.client().stopped_thread() else {
        return Value::Null;
    };
    let stack = session.client().request(
        "stackTrace",
        json!({ "threadId": thread_id, "startFrame": 0, "levels": 1 }),
        DEFAULT_TIMEOUT,
    );
    let Ok(stack) = stack else {
        return Value::Null;
    };
    let frame = &stack["stackFrames"][0];
    json!({
        "id": frame["id"],
        "name": frame["name"],
        "line": frame["line"],
        "column": frame["column"],
        "source": frame["source"]["path"],
        "address": frame["instructionPointerReference"],
    })
}

/// The Kira source line the VM stopped in, when the file is readable.
///
/// The declaration line of the stopped function is the anchor: bytecode
/// carries an instruction index rather than a line, so this names where the
/// function begins and the instruction index says how far into it execution
/// has reached.
fn source_line(target: &kira_debug::PreparedTarget, kira: &kira_debug::VmStop) -> Value {
    let Some(function) = target.function(&kira.function) else {
        return Value::Null;
    };
    let Ok(text) = std::fs::read_to_string(&target.source) else {
        return Value::Null;
    };
    let line = function.line.max(1);
    let Some(content) = text.lines().nth(line as usize - 1) else {
        return Value::Null;
    };
    json!({
        "path": target.source,
        "line": line,
        "text": content.trim_end(),
    })
}

/// Describes a session without asking the target anything.
pub fn session_summary(session: &Session) -> Value {
    json!({
        "session": session.id,
        "state": session.state().label(),
        "backend": session.target.backend,
        "source": session.target.source,
        "executable": session.target.executable,
        "breakpoints": session.breakpoints().len(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use kira_debug::{Backend, DebugFunction, DebugInfo, DebugSource, PreparedTarget, VmStop};
    use std::path::{Path, PathBuf};

    /// A file of this test's own: the tests run in parallel, and one deleting
    /// the file another is still reading is a failure that has nothing to do
    /// with what either of them is checking.
    fn source(test: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "kira-lldb-report-{}-{test}.kira",
            std::process::id()
        ))
    }

    fn target(path: &Path) -> PreparedTarget {
        let info = DebugInfo {
            module_name: "demo".to_owned(),
            backend: Backend::Vm,
            source: Some(DebugSource {
                path: path.to_path_buf(),
            }),
            functions: vec![DebugFunction {
                id: 4,
                name: "discountAmount".to_owned(),
                backend: Backend::Vm,
                symbol: None,
                line: 2,
            }],
            optimized: false,
        };
        PreparedTarget::new(&info, "kira.exe")
    }

    fn stop(function: &str) -> VmStop {
        VmStop {
            function: function.to_owned(),
            function_id: 4,
            ..VmStop::default()
        }
    }

    #[test]
    fn a_stopped_function_is_anchored_to_the_line_it_is_declared_on() {
        let path = source("anchored");
        std::fs::write(&path, "let x = 1\nfunction discountAmount() {\n").expect("source");
        let line = source_line(&target(&path), &stop("discountAmount"));
        let _ = std::fs::remove_file(&path);
        assert_eq!(line["line"], json!(2));
        assert_eq!(line["text"], json!("function discountAmount() {"));
    }

    /// A function the target never described has no line to point at, and
    /// guessing one would send a reader to the wrong place.
    #[test]
    fn an_unknown_function_reports_no_source_line() {
        let path = source("unknown-function");
        std::fs::write(&path, "function main() {}\n").expect("source");
        let line = source_line(&target(&path), &stop("absent"));
        let _ = std::fs::remove_file(&path);
        assert_eq!(line, Value::Null);
    }

    #[test]
    fn a_source_that_cannot_be_read_reports_no_line_rather_than_an_empty_one() {
        let path = PathBuf::from("kira-lldb-report-absent.kira");
        assert_eq!(
            source_line(&target(&path), &stop("discountAmount")),
            Value::Null
        );
    }
}
