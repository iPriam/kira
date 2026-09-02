//! The compiler capability: checking Kira packages held in memory.
//!
//! The VM owns no compiler — it sits below one — so a Kira program that wants
//! to compile Kira describes the package set it wants checked and hands the
//! description to the embedder through
//! [`HostCapabilities::compiler`](crate::HostCapabilities::compiler), exactly as
//! it hands a path to `file_system`. A host that links the frontend answers; a
//! bare embedded VM refuses by name. That is what keeps `kira-vm-runtime`
//! buildable for `wasm32-unknown-unknown`, where there is no compiler to reach.
//!
//! # The unit is a package, not a source string
//!
//! A [`CheckRequest`] carries a *set* of packages, each with its manifest text
//! and its own named files, and names which one is the root. That is the only
//! shape that can express the questions worth asking: imports are file-scoped,
//! so an `import` in one file of a package must not be visible in another; a
//! package is one flat namespace, so two of its files declaring one name
//! collide; and a library plus the app that depends on it is two packages with
//! an edge between them. A single-string API can state none of those.
//!
//! Nothing here names a path on a disk. `path` is what a diagnostic points at
//! and what an import resolves against, and the text beside it is the file.
//!
//! # The wire is a flat string array
//!
//! Both directions cross the engine seam as one `[String]`, because that is the
//! one aggregate every backend already carries across it — the VM builds it out
//! of its heap and native code out of `kira_rt_array_*`, with no new value
//! shape on either side. [`CheckRequest::decode`] and
//! [`CheckDiagnostic::encode`] are the only spelling of that layout, so the
//! Kira side that writes it and the host side that reads it cannot drift.

use std::sync::{Mutex, PoisonError};

use thiserror::Error;

/// A compiler an embedder installed for this process.
///
/// The seam between "a host may be asked to check a package" and "this build
/// actually contains a frontend". Layer 0 cannot contain one — it sits below
/// the lexer — so the capability is a slot, and whoever links a compiler fills
/// it.
pub trait PackageChecker: Send {
    /// Checks one package set, answering with every diagnostic it produced.
    ///
    /// Total, like the capability it backs: a package that does not compile
    /// answers with its problems, never with a failure.
    fn check(&mut self, request: &CheckRequest) -> Vec<CheckDiagnostic>;
}

/// The compiler this process checks with, when one was installed.
///
/// Process-wide rather than per-host for the same reason [`perform`] is: a
/// compiler is a facility of the process, exactly as the filesystem behind
/// [`file_system::perform`](crate::file_system::perform) is, and a program that
/// checks a package from two engines of one hybrid run must reach one of them.
static INSTALLED: Mutex<Option<Box<dyn PackageChecker>>> = Mutex::new(None);

/// Installs the compiler every host in this process answers with.
///
/// Explicit and one-way: a host that is never handed a compiler refuses by
/// name, which is what keeps a bare embedded VM — a browser tab, a test, a
/// sandbox — from silently answering "no diagnostics" for a package it never
/// looked at. Installing again replaces what was installed.
pub fn install(checker: Box<dyn PackageChecker>) {
    let mut installed = INSTALLED.lock().unwrap_or_else(PoisonError::into_inner);
    *installed = Some(checker);
}

/// Runs one request against the installed compiler.
///
/// One function, called from the VM host and from `kira_rt_compiler_*`, because
/// two engines agreeing on what a package compiles to means they must not
/// merely follow the same rules — they must *be* the same rules.
pub fn perform(request: &CheckRequest) -> Result<Vec<CheckDiagnostic>, CompilerError> {
    let mut installed = INSTALLED.lock().unwrap_or_else(PoisonError::into_inner);
    match installed.as_mut() {
        Some(checker) => Ok(checker.check(request)),
        None => Err(CompilerError::NoCompilerHost),
    }
}

/// Which compiler operation one request performs.
///
/// The discriminants are a wire contract: they travel in the operand byte of
/// the `Compiler` bytecode instruction, so they are **append-only** — a new
/// operation takes the next free number and no existing one ever moves.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum CompilerOp {
    /// Check a package set, answering with its diagnostics.
    CheckPackages = 0,
    /// Check the package at a path: the frontend and nothing after it.
    CheckPath = 1,
    /// Build the package at a path: the frontend, then the backend.
    BuildPath = 2,
    /// Build the package at a path and run what was built.
    RunPath = 3,
}

impl CompilerOp {
    /// Every operation, in wire order.
    ///
    /// The one place the set is written down: decoding indexes this rather than
    /// repeating a match, so a new operation cannot be added to the enum and
    /// forgotten by the decoder.
    pub const ALL: [CompilerOp; 4] = [
        CompilerOp::CheckPackages,
        CompilerOp::CheckPath,
        CompilerOp::BuildPath,
        CompilerOp::RunPath,
    ];

    /// The wire byte this operation travels as.
    pub const fn as_byte(self) -> u8 {
        self as u8
    }

    /// Reads a wire byte, or `None` when it names no operation.
    pub fn from_byte(byte: u8) -> Option<Self> {
        Self::ALL.get(usize::from(byte)).copied()
    }

    /// How many operands this operation pops, in source order.
    /// Every operation takes exactly one `[String]`: the request, in the layout
    /// its own type spells. Uniform on purpose — an operation that took a
    /// second operand would need its own lowering, its own stack discipline,
    /// and its own native signature, and there is nothing an extra operand
    /// buys that a field of the request does not.
    pub const fn arity(self) -> usize {
        match self {
            CompilerOp::CheckPackages
            | CompilerOp::CheckPath
            | CompilerOp::BuildPath
            | CompilerOp::RunPath => 1,
        }
    }

    /// The Kira intrinsic name that compiles to this operation.
    pub const fn intrinsic_name(self) -> &'static str {
        match self {
            CompilerOp::CheckPackages => "kcCheckPackages",
            CompilerOp::CheckPath => "kcCheck",
            CompilerOp::BuildPath => "kcBuild",
            CompilerOp::RunPath => "kcRun",
        }
    }

    /// The `kira_rt_*` symbol native code calls to perform this operation.
    ///
    /// Derived from the operation rather than written twice, so the backend's
    /// declaration and the runtime's definition cannot drift apart.
    pub const fn runtime_symbol(self) -> &'static str {
        match self {
            CompilerOp::CheckPackages => "kira_rt_compiler_check_packages",
            CompilerOp::CheckPath => "kira_rt_compiler_check_path",
            CompilerOp::BuildPath => "kira_rt_compiler_build_path",
            CompilerOp::RunPath => "kira_rt_compiler_run_path",
        }
    }

    /// Resolves a Kira intrinsic name to its operation, or `None`.
    pub fn from_intrinsic_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|op| op.intrinsic_name() == name)
    }
}

/// One source file of a package: the name it is known by, and its text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckFile {
    /// The path the file is known by, package-relative (`app/main.kira`).
    ///
    /// Two things at once, and deliberately: it is what a diagnostic points at,
    /// and it is what an `import` inside the package resolves against, under the
    /// same rule a file on disk follows.
    pub path: String,
    /// The file's full source text.
    pub text: String,
}

/// One package of a request: its manifest text and its files.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CheckPackage {
    /// The `package.kira` text, which names the package and its dependencies.
    pub manifest: String,
    /// Every file of the package, in the order the caller listed them.
    pub files: Vec<CheckFile>,
}

/// A package set to check, and which of its packages is the root.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CheckRequest {
    /// Every package taking part, the root among them.
    pub packages: Vec<CheckPackage>,
    /// The name of the package to check — the one whose files are the program.
    ///
    /// The others are its dependencies: their declarations are reachable only
    /// through an `import`, which is the whole point of listing more than one.
    pub root: String,
}

/// The wire tag introducing one package.
const PACKAGE_TAG: &str = "package";

/// The wire tag introducing one file of the package most recently introduced.
const FILE_TAG: &str = "file";

/// How many string slots one encoded diagnostic occupies.
pub const DIAGNOSTIC_FIELDS: usize = 5;

impl CheckRequest {
    /// Reads a request out of the flat string array the seam carries.
    ///
    /// The layout is the root package name, then a stream of records: a
    /// `"package"` tag with its manifest text, and a `"file"` tag with a path
    /// and a text belonging to the package most recently introduced.
    ///
    /// Every malformed input is a typed error rather than a guess: a runtime
    /// that folded an unknown tag into a neighbouring one would check a
    /// different program than the caller wrote.
    pub fn decode(fields: &[String]) -> Result<Self, CheckWireError> {
        let Some(root) = fields.first() else {
            return Err(CheckWireError::NoRoot);
        };
        let mut request = CheckRequest {
            packages: Vec::new(),
            root: root.clone(),
        };
        let mut index = 1;
        while index < fields.len() {
            match fields[index].as_str() {
                PACKAGE_TAG => {
                    let manifest = field(fields, index + 1)?;
                    request.packages.push(CheckPackage {
                        manifest: manifest.clone(),
                        files: Vec::new(),
                    });
                    index += 2;
                }
                FILE_TAG => {
                    let path = field(fields, index + 1)?;
                    let text = field(fields, index + 2)?;
                    let Some(package) = request.packages.last_mut() else {
                        return Err(CheckWireError::FileBeforePackage);
                    };
                    package.files.push(CheckFile {
                        path: path.clone(),
                        text: text.clone(),
                    });
                    index += 3;
                }
                unknown => {
                    return Err(CheckWireError::UnknownTag {
                        tag: unknown.to_owned(),
                    });
                }
            }
        }
        Ok(request)
    }

    /// Writes this request as the flat string array the seam carries.
    ///
    /// The inverse of [`CheckRequest::decode`], and the reason a round-trip test
    /// can hold the two to each other.
    #[must_use]
    pub fn encode(&self) -> Vec<String> {
        let mut fields = vec![self.root.clone()];
        for package in &self.packages {
            fields.push(PACKAGE_TAG.to_owned());
            fields.push(package.manifest.clone());
            for file in &package.files {
                fields.push(FILE_TAG.to_owned());
                fields.push(file.path.clone());
                fields.push(file.text.clone());
            }
        }
        fields
    }
}

/// Reads one field, or reports the truncation that stopped it.
fn field(fields: &[String], index: usize) -> Result<&String, CheckWireError> {
    fields.get(index).ok_or(CheckWireError::Truncated)
}

/// How serious one reported problem is, as the seam spells it.
///
/// The wire form of the compiler's own severity, kept here because this crate
/// is below the diagnostic model and carries no dependency on it. `kira-check`
/// maps between the two in one place.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CheckSeverity {
    /// A hard error: the package does not compile.
    Error,
    /// A warning: the package compiles and something is worth saying.
    Warning,
    /// An informational note.
    Note,
}

impl CheckSeverity {
    /// Every severity, in wire order.
    pub const ALL: [CheckSeverity; 3] = [
        CheckSeverity::Error,
        CheckSeverity::Warning,
        CheckSeverity::Note,
    ];

    /// The text this severity travels as.
    pub const fn as_text(self) -> &'static str {
        match self {
            CheckSeverity::Error => "error",
            CheckSeverity::Warning => "warning",
            CheckSeverity::Note => "note",
        }
    }

    /// Reads a wire spelling, or `None` when it names no severity.
    pub fn from_text(text: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|value| value.as_text() == text)
    }
}

/// One problem the frontend reported, as a value rather than as rendered text.
///
/// Enough to assert on without matching a message: which diagnostic it is, how
/// serious it is, and which file of which package it came from. The message and
/// title are carried for a reader, never for a test to depend on — they get
/// reworded, and codes do not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckDiagnostic {
    /// The diagnostic code (`KSEM061`), empty when the frontend assigned none.
    pub code: String,
    /// How serious it is.
    pub severity: CheckSeverity,
    /// The `path` of the file it points into, empty when it points at no file.
    pub file: String,
    /// The one-line title.
    pub title: String,
    /// The full message.
    pub message: String,
}

impl CheckDiagnostic {
    /// Writes diagnostics as the flat string array the seam carries.
    ///
    /// Fixed stride: each diagnostic occupies [`DIAGNOSTIC_FIELDS`] slots, in
    /// field order, so the reader walks rather than parses.
    #[must_use]
    pub fn encode(diagnostics: &[CheckDiagnostic]) -> Vec<String> {
        let mut fields = Vec::with_capacity(diagnostics.len() * DIAGNOSTIC_FIELDS);
        for diagnostic in diagnostics {
            fields.push(diagnostic.code.clone());
            fields.push(diagnostic.severity.as_text().to_owned());
            fields.push(diagnostic.file.clone());
            fields.push(diagnostic.title.clone());
            fields.push(diagnostic.message.clone());
        }
        fields
    }

    /// Reads diagnostics back out of the flat string array.
    ///
    /// A trailing partial record is refused rather than dropped: a reader that
    /// silently discarded one would report fewer problems than the compiler
    /// found, which is the one failure this whole surface exists to prevent.
    pub fn decode(fields: &[String]) -> Result<Vec<CheckDiagnostic>, CheckWireError> {
        if !fields.len().is_multiple_of(DIAGNOSTIC_FIELDS) {
            return Err(CheckWireError::Truncated);
        }
        fields
            .chunks_exact(DIAGNOSTIC_FIELDS)
            .map(|chunk| {
                let severity = CheckSeverity::from_text(&chunk[1]).ok_or_else(|| {
                    CheckWireError::UnknownTag {
                        tag: chunk[1].clone(),
                    }
                })?;
                Ok(CheckDiagnostic {
                    code: chunk[0].clone(),
                    severity,
                    file: chunk[2].clone(),
                    title: chunk[3].clone(),
                    message: chunk[4].clone(),
                })
            })
            .collect()
    }
}

/// A request the seam could not read.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum CheckWireError {
    /// The array was empty, so it named no root package.
    #[error("a check request must begin with the name of its root package")]
    NoRoot,
    /// A record ran off the end of the array.
    #[error("a check request record is missing fields")]
    Truncated,
    /// A file was listed before any package introduced it.
    #[error("a check request listed a file before any package")]
    FileBeforePackage,
    /// A tag named no record kind.
    #[error("`{tag}` names no check request record")]
    UnknownTag {
        /// The tag as it was written.
        tag: String,
    },
}

/// A compiler operation the host could not even attempt.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum CompilerError {
    /// The host provides no compiler.
    #[error("this host does not provide a compiler")]
    NoCompilerHost,
    /// The request itself could not be read.
    #[error(transparent)]
    Wire(#[from] CheckWireError),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compiler_op_wire_bytes_are_pinned() {
        assert_eq!(CompilerOp::CheckPackages.as_byte(), 0);
        assert_eq!(CompilerOp::CheckPath.as_byte(), 1);
        assert_eq!(CompilerOp::BuildPath.as_byte(), 2);
        assert_eq!(CompilerOp::RunPath.as_byte(), 3);
    }

    #[test]
    fn every_op_round_trips_through_its_byte_and_its_name() {
        for op in CompilerOp::ALL {
            assert_eq!(CompilerOp::from_byte(op.as_byte()), Some(op));
            assert_eq!(
                CompilerOp::from_intrinsic_name(op.intrinsic_name()),
                Some(op)
            );
        }
    }

    #[test]
    fn an_unknown_byte_names_no_operation() {
        assert_eq!(CompilerOp::from_byte(4), None);
        assert_eq!(CompilerOp::from_byte(255), None);
        assert_eq!(CompilerOp::from_intrinsic_name("kcNotAnOperation"), None);
    }

    fn two_package_request() -> CheckRequest {
        CheckRequest {
            root: "App".to_owned(),
            packages: vec![
                CheckPackage {
                    manifest: "Package Core { let kind = .Library }".to_owned(),
                    files: vec![CheckFile {
                        path: "app/Core.kira".to_owned(),
                        text: "function core() -> Int { return 1 }".to_owned(),
                    }],
                },
                CheckPackage {
                    manifest: "Package App { let kind = .App }".to_owned(),
                    files: vec![
                        CheckFile {
                            path: "app/main.kira".to_owned(),
                            text: "import Core".to_owned(),
                        },
                        CheckFile {
                            path: "app/other.kira".to_owned(),
                            text: "function other() { return }".to_owned(),
                        },
                    ],
                },
            ],
        }
    }

    #[test]
    fn a_package_set_round_trips_through_the_wire() {
        let request = two_package_request();
        let decoded = CheckRequest::decode(&request.encode()).expect("decode");
        assert_eq!(decoded, request);
    }

    #[test]
    fn an_empty_array_names_no_root() {
        assert_eq!(CheckRequest::decode(&[]), Err(CheckWireError::NoRoot));
    }

    #[test]
    fn a_file_before_any_package_is_refused() {
        let fields = vec![
            "App".to_owned(),
            "file".to_owned(),
            "a.kira".to_owned(),
            String::new(),
        ];
        assert_eq!(
            CheckRequest::decode(&fields),
            Err(CheckWireError::FileBeforePackage)
        );
    }

    #[test]
    fn a_truncated_record_is_refused_rather_than_guessed() {
        let mut fields = two_package_request().encode();
        fields.pop();
        assert_eq!(
            CheckRequest::decode(&fields),
            Err(CheckWireError::Truncated)
        );
    }

    #[test]
    fn an_unknown_tag_is_refused() {
        let fields = vec!["App".to_owned(), "module".to_owned(), String::new()];
        assert_eq!(
            CheckRequest::decode(&fields),
            Err(CheckWireError::UnknownTag {
                tag: "module".to_owned()
            })
        );
    }

    #[test]
    fn diagnostics_round_trip_through_the_wire() {
        let diagnostics = vec![
            CheckDiagnostic {
                code: "KSEM061".to_owned(),
                severity: CheckSeverity::Error,
                file: "app/FileC.kira".to_owned(),
                title: "unknown name".to_owned(),
                message: "`printLine` is not in scope".to_owned(),
            },
            CheckDiagnostic {
                code: String::new(),
                severity: CheckSeverity::Note,
                file: String::new(),
                title: String::new(),
                message: "a note".to_owned(),
            },
        ];
        let fields = CheckDiagnostic::encode(&diagnostics);
        assert_eq!(fields.len(), diagnostics.len() * DIAGNOSTIC_FIELDS);
        assert_eq!(CheckDiagnostic::decode(&fields), Ok(diagnostics));
    }

    #[test]
    fn a_partial_diagnostic_record_is_refused() {
        let fields = vec!["KSEM061".to_owned(), "error".to_owned()];
        assert_eq!(
            CheckDiagnostic::decode(&fields),
            Err(CheckWireError::Truncated)
        );
    }

    #[test]
    fn every_severity_round_trips_through_its_text() {
        for severity in CheckSeverity::ALL {
            assert_eq!(CheckSeverity::from_text(severity.as_text()), Some(severity));
        }
        assert_eq!(CheckSeverity::from_text("fatal"), None);
    }
}
