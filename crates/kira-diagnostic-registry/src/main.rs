//! Writes the artifacts the diagnostic code table implies, or reports drift.
//!
//! ```text
//! cargo run -p kira-diagnostic-registry -- write
//! cargo run -p kira-diagnostic-registry -- check
//! ```

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use kira_diagnostic_messages::registry;
use kira_diagnostic_registry::{RegistryError, artifacts, emitted_codes};

fn main() -> ExitCode {
    let mut arguments = std::env::args().skip(1);
    let verb = arguments.next().unwrap_or_default();
    if arguments.next().is_some() {
        return usage();
    }
    let repo = repository_root();
    let outcome = match verb.as_str() {
        "write" => write(&repo),
        "check" => check(&repo),
        _ => return usage(),
    };
    match outcome {
        Ok(code) => code,
        Err(error) => {
            eprintln!("kira-diagnostic-registry: {error}");
            ExitCode::FAILURE
        }
    }
}

/// Prints what the tool takes.
fn usage() -> ExitCode {
    eprintln!("usage: kira-diagnostic-registry <write|check>");
    eprintln!("  write  rewrite every artifact from the code table");
    eprintln!("  check  report drift without writing anything");
    ExitCode::from(2)
}

/// The repository root, taken from this crate's place inside it.
fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .unwrap_or(Path::new("."))
        .to_path_buf()
}

/// Rewrites every artifact, naming the ones that changed.
fn write(repo: &Path) -> Result<ExitCode, RegistryError> {
    let mut changed = 0usize;
    for artifact in artifacts() {
        if artifact.write(repo)? {
            println!("wrote {}", artifact.path.display());
            changed += 1;
        }
    }
    println!(
        "{} codes, {changed} of {} artifacts rewritten",
        registry::all().len(),
        artifacts().len()
    );
    if report_unregistered(repo)? == 0 {
        Ok(ExitCode::SUCCESS)
    } else {
        Ok(ExitCode::FAILURE)
    }
}

/// Reports stale artifacts and unregistered codes without writing anything.
fn check(repo: &Path) -> Result<ExitCode, RegistryError> {
    let mut stale = 0usize;
    for artifact in artifacts() {
        if !artifact.is_current(repo)? {
            eprintln!("stale: {}", artifact.path.display());
            stale += 1;
        }
    }
    if stale == 0 && report_unregistered(repo)? == 0 {
        println!("{} codes, every artifact current", registry::all().len());
        return Ok(ExitCode::SUCCESS);
    }
    Ok(ExitCode::FAILURE)
}

/// Reports codes the toolchain emits that the table does not list, and rows
/// the table lists that nothing emits.
fn report_unregistered(repo: &Path) -> Result<usize, RegistryError> {
    let emitted = emitted_codes(repo)?;
    let mut wrong = 0usize;
    for (code, origin) in &emitted {
        if !registry::contains(code) {
            eprintln!("emitted but not registered: {code} ({})", origin.display());
            wrong += 1;
        }
    }
    for entry in registry::all() {
        if !emitted.contains_key(entry.code) {
            eprintln!("registered but not emitted: {}", entry.code);
            wrong += 1;
        }
    }
    Ok(wrong)
}
