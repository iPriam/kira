//! The toolchain capability: checking, building, and running a package on disk.
//!
//! The sibling of [`compiler`](crate::compiler), and deliberately not the same
//! thing. That capability answers a question about source a program is holding:
//! the package set arrives as text, nothing is read and nothing is written, and
//! the answer is a list of diagnostics. This one answers a question about a
//! project that is already on a disk — a directory with a `package.kira` in it
//! — and building or running one means producing artifacts and starting a
//! program.
//!
//! Both are one capability slot each rather than one shared slot, because a
//! host can honestly have one and not the other. A browser tab embeds the
//! frontend and can check a package set held in memory; it has no directory to
//! build and no process to start. Splitting them lets that host answer what it
//! can and refuse what it cannot, instead of refusing both.
//!
//! # No process is spawned by the caller
//!
//! A Kira program calling `kcRun` is not shelling out to `kira run`. It hands
//! the request to whoever installed a [`Toolchain`], and that implementation
//! drives the same build the CLI drives. What the program gets back is the exit
//! code and the diagnostics as values, never rendered text it would have to
//! parse.
//!
//! # The wire is a flat string array
//!
//! The same one [`CheckRequest`](crate::CheckRequest) uses, for the same
//! reason: it is the one aggregate every backend already carries across the
//! engine seam. [`ToolRequest::encode`] and [`ToolAnswer::encode`] are the only
//! spelling of the layout, so the Kira side that writes it and the host side
//! that reads it cannot drift.

use std::sync::{Mutex, PoisonError};

use thiserror::Error;

use crate::compiler::{CheckSeverity, CompilerOp};

/// Which backend a request is for.
///
/// `Hybrid` is two entries rather than one with a flag, because the bias is
/// part of naming the backend: a hybrid build that leans on the runtime and one
/// that leans on native code are two different programs, and a caller picks
/// between them the same way it picks between the VM and native code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ToolBackend {
    /// The interpreter: bytecode, run in this process.
    Vm,
    /// Native code: an object, linked into an executable.
    Native,
    /// Both halves, leaning on the runtime where either would serve.
    HybridRuntimeBias,
    /// Both halves, leaning on native code where either would serve.
    HybridNativeBias,
}

impl ToolBackend {
    /// Every backend, in wire order.
    pub const ALL: [ToolBackend; 4] = [
        ToolBackend::Vm,
        ToolBackend::Native,
        ToolBackend::HybridRuntimeBias,
        ToolBackend::HybridNativeBias,
    ];

    /// The text this backend travels as.
    pub const fn as_text(self) -> &'static str {
        match self {
            ToolBackend::Vm => "vm",
            ToolBackend::Native => "native",
            ToolBackend::HybridRuntimeBias => "hybrid-runtime",
            ToolBackend::HybridNativeBias => "hybrid-native",
        }
    }

    /// Reads a wire spelling, or `None` when it names no backend.
    pub fn from_text(text: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|value| value.as_text() == text)
    }
}

/// One variable the program sees in its environment when it is run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolVariable {
    /// The variable's name.
    pub name: String,
    /// Its value.
    pub value: String,
}

/// What one toolchain operation was asked to do.
///
/// One request type for all three verbs rather than three, because they differ
/// only in how far they go: `check` stops at the frontend, `build` continues
/// into the backend, and `run` starts what it built. A check ignores
/// `environment`, which is the honest reading — nothing it does can observe it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolRequest {
    /// The package directory, or the `.kira` file, to work on.
    pub path: String,
    /// The backend to compile with, and to run on.
    pub backend: ToolBackend,
    /// The environment a run program sees.
    pub environment: Vec<ToolVariable>,
}

impl Default for ToolRequest {
    fn default() -> Self {
        ToolRequest {
            path: String::new(),
            backend: ToolBackend::Vm,
            environment: Vec::new(),
        }
    }
}

/// How many string slots one encoded diagnostic occupies.
pub const TOOL_DIAGNOSTIC_FIELDS: usize = 3;

/// How many string slots precede the diagnostics in an answer.
const ANSWER_HEADER_FIELDS: usize = 2;

/// The status field of an answer a toolchain produced.
const STATUS_OK: &str = "ok";

/// The status field of an answer no toolchain produced.
const STATUS_NO_HOST: &str = "no-host";

impl ToolRequest {
    /// Writes this request as the flat string array the seam carries.
    ///
    /// The layout is the path, the backend's text, then a name and a value per
    /// environment variable. Fixed stride after the header, so the reader walks
    /// rather than parses.
    #[must_use]
    pub fn encode(&self) -> Vec<String> {
        let mut fields = vec![self.path.clone(), self.backend.as_text().to_owned()];
        for variable in &self.environment {
            fields.push(variable.name.clone());
            fields.push(variable.value.clone());
        }
        fields
    }

    /// Reads a request out of the flat string array the seam carries.
    ///
    /// Every malformed input is a typed error rather than a guess: a half
    /// variable is not a variable, and a backend this build does not know must
    /// not quietly become the default one — that would build a different
    /// program than the caller asked for.
    pub fn decode(fields: &[String]) -> Result<Self, ToolWireError> {
        let Some(path) = fields.first() else {
            return Err(ToolWireError::NoPath);
        };
        let Some(backend) = fields.get(1) else {
            return Err(ToolWireError::NoBackend);
        };
        let backend = ToolBackend::from_text(backend).ok_or_else(|| ToolWireError::UnknownBackend {
            backend: backend.clone(),
        })?;
        let variables = &fields[ANSWER_HEADER_FIELDS..];
        if !variables.len().is_multiple_of(2) {
            return Err(ToolWireError::Truncated);
        }
        Ok(ToolRequest {
            path: path.clone(),
            backend,
            environment: variables
                .chunks_exact(2)
                .map(|pair| ToolVariable {
                    name: pair[0].clone(),
                    value: pair[1].clone(),
                })
                .collect(),
        })
    }
}

/// One problem the toolchain reported, as a value rather than rendered text.
///
/// A code, a line, and how serious it is: what an assertion is written against.
/// No message, on purpose — messages get reworded and codes do not, so a test
/// that matched text would break on an edit that changed nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolDiagnostic {
    /// The diagnostic code (`KSEM061`), empty when the toolchain assigned none.
    pub code: String,
    /// The line it points at, `0` when it points at no line.
    pub line: i64,
    /// How serious it is.
    pub severity: CheckSeverity,
}

/// What one toolchain operation answered.
///
/// `exit_code` is meaningful only for a run that started, and only when no
/// diagnostic is an error: a package that did not build has no code to report,
/// and a zero there would read as a program that ran and succeeded.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ToolAnswer {
    /// The exit code of the program that ran, `0` when none did.
    pub exit_code: i64,
    /// Every problem reported, in the order the toolchain reported them.
    pub diagnostics: Vec<ToolDiagnostic>,
}

impl ToolAnswer {
    /// Writes an answer as the flat string array the seam carries.
    ///
    /// The layout is a status, an exit code, then a fixed-stride record per
    /// diagnostic. The status is first because the one thing a caller must
    /// never do is read a refusal as an empty diagnostic list, which says the
    /// package compiled.
    #[must_use]
    pub fn encode(&self) -> Vec<String> {
        let mut fields = Vec::with_capacity(
            ANSWER_HEADER_FIELDS + self.diagnostics.len() * TOOL_DIAGNOSTIC_FIELDS,
        );
        fields.push(STATUS_OK.to_owned());
        fields.push(self.exit_code.to_string());
        for diagnostic in &self.diagnostics {
            fields.push(diagnostic.code.clone());
            fields.push(diagnostic.line.to_string());
            fields.push(diagnostic.severity.as_text().to_owned());
        }
        fields
    }

    /// Writes the answer a host with no toolchain gives.
    ///
    /// Carries the refusal in the status rather than in a diagnostic, so the
    /// Kira side decides how to say it and the wire says only what happened.
    #[must_use]
    pub fn encode_refusal() -> Vec<String> {
        vec![STATUS_NO_HOST.to_owned(), "0".to_owned()]
    }

    /// Reads an answer back out of the flat string array.
    ///
    /// A trailing partial record is refused rather than dropped: a reader that
    /// discarded one would report fewer problems than the toolchain found,
    /// which is the failure this whole surface exists to prevent.
    pub fn decode(fields: &[String]) -> Result<Self, ToolWireError> {
        let Some(status) = fields.first() else {
            return Err(ToolWireError::Truncated);
        };
        if status == STATUS_NO_HOST {
            return Err(ToolWireError::NoToolchainHost);
        }
        if status != STATUS_OK {
            return Err(ToolWireError::UnknownStatus {
                status: status.clone(),
            });
        }
        let Some(exit_code) = fields.get(1) else {
            return Err(ToolWireError::Truncated);
        };
        let exit_code = exit_code
            .parse::<i64>()
            .map_err(|_| ToolWireError::UnreadableExitCode {
                text: exit_code.clone(),
            })?;
        let records = &fields[ANSWER_HEADER_FIELDS..];
        if !records.len().is_multiple_of(TOOL_DIAGNOSTIC_FIELDS) {
            return Err(ToolWireError::Truncated);
        }
        let diagnostics = records
            .chunks_exact(TOOL_DIAGNOSTIC_FIELDS)
            .map(|chunk| {
                let severity = CheckSeverity::from_text(&chunk[2]).ok_or_else(|| {
                    ToolWireError::UnknownSeverity {
                        severity: chunk[2].clone(),
                    }
                })?;
                Ok(ToolDiagnostic {
                    code: chunk[0].clone(),
                    line: chunk[1].parse::<i64>().unwrap_or(0),
                    severity,
                })
            })
            .collect::<Result<Vec<ToolDiagnostic>, ToolWireError>>()?;
        Ok(ToolAnswer {
            exit_code,
            diagnostics,
        })
    }
}

/// Which of the three verbs one operation performs.
///
/// Named separately from [`CompilerOp`] so an implementation of [`Toolchain`]
/// matches on the three things it can be asked, and never on the in-memory
/// check that is not its job.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ToolVerb {
    /// Check the package: the frontend and nothing after it.
    Check,
    /// Build the package: the frontend, then the backend.
    Build,
    /// Build the package and run what was built.
    Run,
}

/// A toolchain an embedder installed for this process.
pub trait Toolchain: Send {
    /// Performs one verb, answering with what it found and what it did.
    ///
    /// Total, like the capability it backs: a package that does not compile
    /// answers with its problems, never with a failure. The one thing that is
    /// not an answer is having no toolchain at all, and that is decided before
    /// this is ever reached.
    fn perform(&mut self, verb: ToolVerb, request: &ToolRequest) -> ToolAnswer;
}

/// The toolchain this process works through, when one was installed.
///
/// Process-wide for the same reason the compiler slot is: a toolchain is a
/// facility of the process, and a program driving one from two engines of a
/// hybrid run must reach the same one.
static INSTALLED: Mutex<Option<Box<dyn Toolchain>>> = Mutex::new(None);

/// Installs the toolchain every host in this process answers with.
///
/// Explicit and one-way, which is what keeps an embedded VM — a browser tab, a
/// test, a sandbox — from silently answering "no diagnostics" for a package it
/// never built. Installing again replaces what was installed.
pub fn install(toolchain: Box<dyn Toolchain>) {
    let mut installed = INSTALLED.lock().unwrap_or_else(PoisonError::into_inner);
    *installed = Some(toolchain);
}

/// Runs one request against the installed toolchain.
pub fn perform(verb: ToolVerb, request: &ToolRequest) -> Result<ToolAnswer, ToolchainError> {
    let mut installed = INSTALLED.lock().unwrap_or_else(PoisonError::into_inner);
    match installed.as_mut() {
        Some(toolchain) => Ok(toolchain.perform(verb, request)),
        None => Err(ToolchainError::NoToolchainHost),
    }
}

impl CompilerOp {
    /// The verb this operation performs, or `None` when it performs none.
    ///
    /// [`CompilerOp::CheckPackages`] answers `None`: it is the in-memory
    /// capability, and it reaches a different slot.
    pub const fn verb(self) -> Option<ToolVerb> {
        match self {
            CompilerOp::CheckPackages => None,
            CompilerOp::CheckPath => Some(ToolVerb::Check),
            CompilerOp::BuildPath => Some(ToolVerb::Build),
            CompilerOp::RunPath => Some(ToolVerb::Run),
        }
    }
}

/// A request or an answer the seam could not read.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ToolWireError {
    /// The array was empty, so it named no package to work on.
    #[error("a toolchain request must begin with a path")]
    NoPath,
    /// The array named a path and stopped.
    #[error("a toolchain request must name a backend")]
    NoBackend,
    /// A record ran off the end of the array.
    #[error("a toolchain record is missing fields")]
    Truncated,
    /// The backend text named no backend this build knows.
    #[error("`{backend}` names no backend")]
    UnknownBackend {
        /// The backend as it was written.
        backend: String,
    },
    /// The severity text named no severity this build knows.
    #[error("`{severity}` names no severity")]
    UnknownSeverity {
        /// The severity as it was written.
        severity: String,
    },
    /// The status text named no status this build knows.
    #[error("`{status}` names no answer status")]
    UnknownStatus {
        /// The status as it was written.
        status: String,
    },
    /// The exit code was not a number.
    #[error("`{text}` is not an exit code")]
    UnreadableExitCode {
        /// The exit code as it was written.
        text: String,
    },
    /// The answer says no toolchain performed it.
    #[error("this host does not provide a toolchain")]
    NoToolchainHost,
}

/// A toolchain operation the host could not even attempt.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ToolchainError {
    /// The host provides no toolchain.
    #[error("this host does not provide a toolchain")]
    NoToolchainHost,
    /// The request itself could not be read.
    #[error(transparent)]
    Wire(#[from] ToolWireError),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request() -> ToolRequest {
        ToolRequest {
            path: "/projects/App".to_owned(),
            backend: ToolBackend::HybridNativeBias,
            environment: vec![
                ToolVariable {
                    name: "KIRA_LOG".to_owned(),
                    value: "debug".to_owned(),
                },
                ToolVariable {
                    name: "EMPTY".to_owned(),
                    value: String::new(),
                },
            ],
        }
    }

    fn answer() -> ToolAnswer {
        ToolAnswer {
            exit_code: 3,
            diagnostics: vec![
                ToolDiagnostic {
                    code: "KSEM061".to_owned(),
                    line: 12,
                    severity: CheckSeverity::Error,
                },
                ToolDiagnostic {
                    code: String::new(),
                    line: 0,
                    severity: CheckSeverity::Note,
                },
            ],
        }
    }

    #[test]
    fn a_request_round_trips_through_the_wire() {
        let request = request();
        assert_eq!(ToolRequest::decode(&request.encode()), Ok(request));
    }

    #[test]
    fn an_answer_round_trips_through_the_wire() {
        let answer = answer();
        assert_eq!(ToolAnswer::decode(&answer.encode()), Ok(answer));
    }

    #[test]
    fn every_backend_round_trips_through_its_text() {
        for backend in ToolBackend::ALL {
            assert_eq!(ToolBackend::from_text(backend.as_text()), Some(backend));
        }
        assert_eq!(ToolBackend::from_text("interpreter"), None);
    }

    #[test]
    fn an_empty_request_names_no_path() {
        assert_eq!(ToolRequest::decode(&[]), Err(ToolWireError::NoPath));
    }

    #[test]
    fn a_request_naming_no_backend_is_refused() {
        let fields = vec!["/projects/App".to_owned()];
        assert_eq!(ToolRequest::decode(&fields), Err(ToolWireError::NoBackend));
    }

    #[test]
    fn an_unknown_backend_is_refused_rather_than_defaulted() {
        let fields = vec!["/projects/App".to_owned(), "interpreter".to_owned()];
        assert_eq!(
            ToolRequest::decode(&fields),
            Err(ToolWireError::UnknownBackend {
                backend: "interpreter".to_owned()
            })
        );
    }

    #[test]
    fn a_half_variable_is_refused() {
        let mut fields = request().encode();
        fields.pop();
        assert_eq!(ToolRequest::decode(&fields), Err(ToolWireError::Truncated));
    }

    #[test]
    fn a_partial_diagnostic_record_is_refused() {
        let mut fields = answer().encode();
        fields.pop();
        assert_eq!(ToolAnswer::decode(&fields), Err(ToolWireError::Truncated));
    }

    /// A refusal must not decode as an answer with no diagnostics, which is
    /// what "it compiled" looks like.
    #[test]
    fn a_refusal_is_not_an_empty_answer() {
        assert_eq!(
            ToolAnswer::decode(&ToolAnswer::encode_refusal()),
            Err(ToolWireError::NoToolchainHost)
        );
    }

    #[test]
    fn only_the_path_operations_name_a_verb() {
        assert_eq!(CompilerOp::CheckPackages.verb(), None);
        assert_eq!(CompilerOp::CheckPath.verb(), Some(ToolVerb::Check));
        assert_eq!(CompilerOp::BuildPath.verb(), Some(ToolVerb::Build));
        assert_eq!(CompilerOp::RunPath.verb(), Some(ToolVerb::Run));
    }

    /// A host that was never given a toolchain says so, rather than answering
    /// with no diagnostics.
    #[test]
    fn a_process_with_no_toolchain_refuses_by_name() {
        // This test runs in a process where nothing installed one; a test that
        // installs a toolchain would have to live in its own binary, because the
        // slot is process-wide by design.
        if INSTALLED
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .is_none()
        {
            assert_eq!(
                perform(ToolVerb::Check, &ToolRequest::default()),
                Err(ToolchainError::NoToolchainHost)
            );
        }
    }
}
