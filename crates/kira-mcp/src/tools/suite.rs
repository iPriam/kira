//! Choosing and running Kira's own tests, with failures as structured results.
//!
//! Not a tool of its own. `kira_dev_validate` is the one entry point to the
//! gate, and the tests are one of the things it runs — so what used to be a
//! second tool is the part of that one that decides *which* tests and reads
//! what they printed. Keeping it in its own file keeps the gate's own file
//! about the gate.

use std::time::Duration;

use serde_json::{Value, json};

use super::{BACKENDS, bool_field, enum_field, environment, string_field};
use crate::exec::{self, Run};
use crate::schema::{Diagnostic, Failure, FailureKind};

pub const SUITES: [&str; 15] = [
    "all",
    "unit",
    "integration",
    "compiler",
    "diagnostics",
    "golden",
    "parser",
    "semantic",
    "lowering",
    "vm",
    "llvm",
    "hybrid",
    "backend_parity",
    "runtime",
    "toolchain",
];

/// How each suite selects work: the packages it runs, and the name filter.
///
/// A suite is a *question about the toolchain*, not a cargo invocation. The
/// mapping lives here so an agent asks "does lowering still pass" rather than
/// remembering which crate holds those tests.
fn selection(suite: &str) -> (&'static [&'static str], Option<&'static str>) {
    match suite {
        "unit" => (&[], None),
        "parser" => (&["kira-parser", "kira-lexer"], None),
        "semantic" => (&["kira-semantics", "kira-semantics-model"], None),
        "lowering" => (&["kira-ir", "kira-bytecode"], None),
        "vm" => (&["kira-vm-runtime"], None),
        "llvm" => (&["kira-llvm-backend"], None),
        "compiler" => (
            &[
                "kira-lexer",
                "kira-parser",
                "kira-semantics",
                "kira-ir",
                "kira-bytecode",
            ],
            None,
        ),
        "runtime" => (
            &["kira-vm-runtime", "kira-native-bridge", "kira-main"],
            None,
        ),
        "toolchain" => (
            &["kira-cli", "kira-build", "kira-project", "kira-knvm"],
            None,
        ),
        "diagnostics" => (&["kira-diagnostics", "kira-diagnostic-messages"], None),
        "hybrid" => (&["kira-hybrid-runtime", "kira-cli"], Some("seam")),
        "backend_parity" => (&["kira-cli"], Some("backend_parity")),
        "golden" => (&["kira-cli"], Some("shader_validation")),
        "integration" => (&["kira-cli"], None),
        _ => (&[], None),
    }
}

/// The fields `kira_dev_validate` advertises for choosing what to run.
///
/// There is no `test` field for a test *name* — `test` is the boolean that
/// decides whether tests run at all — so a single test is named through
/// `filter`, which always did the same job.
pub fn schema_properties() -> Value {
    json!({
        "suite": { "type": "string", "enum": SUITES },
        "package": { "type": "string" },
        "crate": { "type": "string" },
        "file": {
            "type": "string",
            "description": "A path under `crates/`. Runs that crate's tests, filtered to the file's own name — the way to check one module without knowing which package owns it."
        },
        "filter": { "type": "string" },
        "backend": { "type": "string", "enum": BACKENDS },
        "profile": { "type": "string", "enum": ["debug", "release"] },
        "update_snapshots": { "type": "boolean" }
    })
}

/// What to run when the caller named neither a suite nor a place.
pub enum Fallback {
    /// Every test in the workspace.
    Workspace,
    /// Only these packages, which is how a changed-file list narrows the gate.
    Packages(Vec<String>),
}

/// The counts across every test binary in one run.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Tally {
    pub passed: u64,
    pub failed: u64,
    pub skipped: u64,
}

/// One test run, read.
pub struct Outcome {
    /// What ran, for the gate's list of checks.
    pub name: String,
    pub run: Run,
    pub tally: Tally,
    pub failures: Vec<Failure>,
    pub diagnostics: Vec<Diagnostic>,
}

impl Outcome {
    pub fn success(&self) -> bool {
        self.run.success()
    }
}

/// Runs the tests `arguments` selects, falling back to `fallback`.
pub fn run(
    arguments: &Value,
    bound: Duration,
    fallback: Fallback,
) -> Result<Outcome, (Value, bool)> {
    let suite = enum_field(arguments, "suite", &SUITES, None)?;
    let profile =
        enum_field(arguments, "profile", &["debug", "release"], Some("debug"))?.unwrap_or("debug");
    let mut env = environment(arguments)?;
    if bool_field(arguments, "update_snapshots", false)? {
        env.push(("UPDATE_EXPECT".to_owned(), "1".to_owned()));
    }
    let named = string_field(arguments, "package")?
        .or(string_field(arguments, "crate")?)
        .map(str::to_owned);
    let mut filter = string_field(arguments, "filter")?.map(str::to_owned);

    // A file names both a package and a filter, which is what makes it the
    // shorthand it is: neither has to be known to check one module.
    let file = string_field(arguments, "file")?;
    let from_file = file.and_then(package_of);
    if filter.is_none() {
        filter = file.and_then(file_stem);
    }
    let named = named.or(from_file);

    let mut args = vec!["test".to_owned()];
    match (suite, &named) {
        (_, Some(name)) => {
            args.push("-p".to_owned());
            args.push(name.clone());
        }
        (Some("all"), None) => args.push("--workspace".to_owned()),
        (Some("unit"), None) => {
            args.push("--workspace".to_owned());
            args.push("--lib".to_owned());
        }
        (Some(suite), None) => {
            for package in selection(suite).0 {
                args.push("-p".to_owned());
                args.push((*package).to_owned());
            }
        }
        (None, None) => match fallback {
            Fallback::Workspace => args.push("--workspace".to_owned()),
            Fallback::Packages(packages) => {
                for package in packages {
                    args.push("-p".to_owned());
                    args.push(package);
                }
            }
        },
    }
    if profile == "release" {
        args.push("--release".to_owned());
    }
    // Never let the first failing binary hide the rest: a suite that stopped
    // early reports the binaries it never ran as nothing at all.
    args.push("--no-fail-fast".to_owned());
    if let Some(filter) = filter
        .as_deref()
        .or_else(|| suite.and_then(|s| selection(s).1))
    {
        args.push("--".to_owned());
        args.push(filter.to_owned());
    }

    let name = match (suite, &named) {
        (_, Some(package)) => format!("tests_{package}"),
        (Some(suite), None) => format!("tests_{suite}"),
        (None, None) => "tests".to_owned(),
    };
    let root = exec::repository_root();
    let run = match exec::run("cargo", &args, &root, &env, bound) {
        Ok(run) => run,
        Err(error) => {
            let failure = Failure::new(FailureKind::Crash, error.to_string());
            return Err((
                json!({ "success": false, "failures": [failure], "stdout": "", "stderr": "" }),
                true,
            ));
        }
    };

    let tally = parse_tally(&run.stdout);
    let failures = parse_failures(&run);
    let diagnostics = diagnostics_for(&run, run.success(), tally.failed);
    Ok(Outcome {
        name,
        run,
        tally,
        failures,
        diagnostics,
    })
}

/// The crate a path under `crates/` belongs to.
pub fn package_of(path: &str) -> Option<String> {
    let normalized = path.replace('\\', "/");
    let package = normalized.split("crates/").nth(1)?.split('/').next()?;
    (!package.is_empty()).then(|| package.to_owned())
}

/// A path's file name without its extension, as a test-name filter.
fn file_stem(path: &str) -> Option<String> {
    let normalized = path.replace('\\', "/");
    let name = normalized.rsplit('/').next()?;
    let stem = name.split('.').next()?;
    (!stem.is_empty()).then(|| stem.to_owned())
}

/// Sums the per-binary `test result:` lines.
///
/// Summed rather than taken from the last line: `cargo test` prints one result
/// line per binary, and reading only the last reports the last binary's tally
/// as the whole run's.
fn parse_tally(stdout: &str) -> Tally {
    let mut tally = Tally::default();
    for line in stdout.lines() {
        let Some(rest) = line.strip_prefix("test result:") else {
            continue;
        };
        let words: Vec<&str> = rest.split_whitespace().collect();
        for pair in words.windows(2) {
            let Ok(count) = pair[0].parse::<u64>() else {
                continue;
            };
            match pair[1] {
                "passed;" | "passed" => tally.passed += count,
                "failed;" | "failed" => tally.failed += count,
                "ignored;" | "ignored" => tally.skipped += count,
                _ => {}
            }
        }
    }
    tally
}

/// One structured failure per failing test name.
fn parse_failures(run: &Run) -> Vec<Failure> {
    let mut failures: Vec<Failure> = Vec::new();
    for line in run.stdout.lines() {
        let Some(name) = line
            .strip_prefix("test ")
            .and_then(|rest| rest.strip_suffix(" ... FAILED"))
        else {
            continue;
        };
        if name.starts_with("result:") {
            continue;
        }
        let mut failure = Failure::new(FailureKind::TestFailure, format!("`{name}` failed"));
        failure.command = Some(run.command.clone());
        failure.exit_code = run.exit_code;
        failure.backtrace = panic_of(&run.stdout, name);
        failures.push(failure);
    }
    if failures.is_empty() && !run.success() {
        // The run failed and named no test: a compile error, a harness crash,
        // or a timeout. Reporting zero failures here would read as a pass.
        let kind = match run.timed_out {
            true => FailureKind::Timeout,
            false => FailureKind::Crash,
        };
        failures
            .push(Failure::new(kind, "the test run failed without naming a test").with_run(run));
    }
    failures
}

/// The panic block a named test printed, when it printed one.
fn panic_of(stdout: &str, name: &str) -> Option<String> {
    let header = format!("---- {name} stdout ----");
    let start = stdout.find(&header)? + header.len();
    let rest = &stdout[start..];
    let end = rest.find("\n---- ").unwrap_or(rest.len());
    let block = rest[..end].trim();
    (!block.is_empty()).then(|| block.to_owned())
}

/// A diagnostic when the run failed in a way the tally cannot express.
fn diagnostics_for(run: &Run, success: bool, failed: u64) -> Vec<Diagnostic> {
    if success || failed > 0 {
        return Vec::new();
    }
    vec![Diagnostic::message(
        "error",
        match run.timed_out {
            true => "the test run timed out before reporting a tally".to_owned(),
            false => format!(
                "the test run exited {:?} without reporting a failing test; \
                 read `stderr` for a compile or harness error",
                run.exit_code
            ),
        },
    )]
}

#[cfg(test)]
#[path = "suite_tests.rs"]
mod tests;
