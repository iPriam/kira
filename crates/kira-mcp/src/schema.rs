//! The diagnostic and failure shapes every tool returns.
//!
//! One shape, everywhere: a caller that learned to read a diagnostic from
//! `kira_dev_build` reads the same fields out of `kira_dev_test` and
//! `kira_dev_fuzz`. Two shapes would mean every consumer carrying two readers
//! and guessing which applies.

use serde::Serialize;
use serde_json::{Value, json};

/// Where in a file a diagnostic points.
#[derive(Debug, Clone, Serialize)]
pub struct Span {
    pub start_line: u32,
    pub start_column: u32,
    pub end_line: u32,
    pub end_column: u32,
}

/// One diagnostic, from the Kira compiler or from cargo.
#[derive(Debug, Clone, Serialize)]
pub struct Diagnostic {
    pub severity: String,
    pub code: Option<String>,
    pub message: String,
    pub file: Option<String>,
    pub span: Option<Span>,
    pub notes: Vec<String>,
    pub suggestions: Vec<String>,
    pub related_diagnostics: Vec<Diagnostic>,
    pub compiler_stage: Option<String>,
    pub backend: Option<String>,
}

impl Diagnostic {
    /// A diagnostic with only the fields a plain message carries.
    pub fn message(severity: &str, message: impl Into<String>) -> Diagnostic {
        Diagnostic {
            severity: severity.to_owned(),
            code: None,
            message: message.into(),
            file: None,
            span: None,
            notes: Vec::new(),
            suggestions: Vec::new(),
            related_diagnostics: Vec::new(),
            compiler_stage: None,
            backend: None,
        }
    }
}

/// What kind of thing went wrong.
///
/// A closed set, so a consumer can branch on it exhaustively rather than
/// matching on message text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureKind {
    Diagnostic,
    TestFailure,
    Panic,
    Crash,
    Timeout,
    Hang,
    BackendDivergence,
    InvalidArtifact,
    PerformanceRegression,
    /// The repository has no implementation of the thing that was asked for.
    ///
    /// Not in the shared vocabulary the spec fixes, and deliberately added:
    /// answering "no fuzz targets exist here" with `Crash` would be a lie, and
    /// answering with success would be worse. A caller can tell "this failed"
    /// from "this cannot be done here" only if the two have different names.
    CapabilityMissing,
}

/// One failure, in the shape every tool reports failures in.
#[derive(Debug, Clone, Serialize)]
pub struct Failure {
    pub kind: FailureKind,
    pub message: String,
    pub stage: Option<String>,
    pub backend: Option<String>,
    pub target: Option<String>,
    pub command: Option<Vec<String>>,
    pub exit_code: Option<i32>,
    pub signal: Option<i32>,
    pub backtrace: Option<String>,
    pub stdout: String,
    pub stderr: String,
    pub artifacts: Vec<String>,
    pub reproduction: Option<Value>,
}

impl Failure {
    /// A failure with the fields a message-only report carries.
    pub fn new(kind: FailureKind, message: impl Into<String>) -> Failure {
        Failure {
            kind,
            message: message.into(),
            stage: None,
            backend: None,
            target: None,
            command: None,
            exit_code: None,
            signal: None,
            backtrace: None,
            stdout: String::new(),
            stderr: String::new(),
            artifacts: Vec::new(),
            reproduction: None,
        }
    }

    /// Attaches the command and output of the run that produced it.
    pub fn with_run(mut self, run: &crate::exec::Run) -> Failure {
        self.command = Some(run.command.clone());
        self.exit_code = run.exit_code;
        self.stdout = run.stdout.clone();
        self.stderr = run.stderr.clone();
        self
    }
}

/// The result a tool returns when the repository cannot do what was asked.
///
/// Reported as an error result, never as a success with an empty body: a caller
/// that read zero crashes from a fuzz run that never happened would conclude the
/// fuzzer found nothing.
pub fn capability_missing(what: &str, detail: &str) -> (Value, bool) {
    let failure = Failure::new(FailureKind::CapabilityMissing, detail.to_owned());
    (
        json!({
            "success": false,
            "capability": what,
            "failures": [failure],
            "diagnostics": [Diagnostic::message("error", detail)],
            "stdout": "",
            "stderr": "",
        }),
        true,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_capability_is_an_error_result_not_an_empty_success() {
        let (value, is_error) = capability_missing("fuzz", "no fuzz targets exist in this repo");
        assert!(is_error);
        assert_eq!(value["success"], json!(false));
        assert_eq!(value["failures"][0]["kind"], json!("capability_missing"));
    }

    /// The failure kinds serialize as the snake_case names the schema fixes.
    #[test]
    fn failure_kinds_serialize_as_their_schema_names() {
        let rendered = serde_json::to_string(&FailureKind::BackendDivergence).expect("json");
        assert_eq!(rendered, "\"backend_divergence\"");
    }
}
