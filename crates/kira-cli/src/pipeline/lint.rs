//! `kira lint`: run a package's `linter.kira` and, with `--fix`, apply what it
//! offers.
//!
//! Split from the other verbs because it is the one that *writes source back*.
//! Everything else in the pipeline reads a package and reports; applying a fix
//! edits the files the diagnostics point at, and that deserves to be read on its
//! own rather than in the middle of the build verbs.

use kira_diagnostics::{Diagnostic, Suggestion};
use kira_source::{SourceId, SourceMap};

use crate::progress::out;

use super::{EXIT_FAILURE, EXIT_OK, compile, compile_target};

/// Runs `kira lint <file|dir>`: report what the package's lints found.
///
/// Closer to `check` than to `test`. A lint runs during *expansion* — the
/// `LintRunner` collector is handed every declaration and reports as it goes —
/// so there is nothing to execute afterwards and no backend to pick. Compiling
/// the package is the whole of the work; this only decides what to print and
/// what to exit with.
///
/// A lint that warns does not fail the run, because a warning is an opinion
/// about code that already compiles. Only an error does, which is what a
/// `linter.kira` entry asks for when it writes `severity = "error"`.
pub fn lint(args: &[String]) -> i32 {
    let surface = crate::progress::Surface::install("Linting");
    let _guard = crate::progress::Finish(surface);
    let apply = args.iter().any(|arg| arg == "--fix");
    let path = args
        .iter()
        .find(|arg| !arg.starts_with("--"))
        .map(String::as_str)
        .unwrap_or(crate::options::DEFAULT_PATH);
    // Set before anything is compiled, because the frontend reads it once at the
    // edge and turns it into a salsa input. This is the whole of what tells the
    // lint runner it was asked for; every other verb leaves it unset and the
    // runner returns without looking at a single declaration.
    //
    // SAFETY: single-threaded, before any thread that could read the
    // environment is started — the compile below is the first thing that does.
    unsafe { std::env::set_var(kira_build::frontend::LINT_MODE, "1") };
    match compile(path, &compile_target(path, None)) {
        Ok(compiled) => {
            // Only what lives under the path being linted. A collector is
            // handed every declaration in the *program*, dependencies included,
            // so a lint configured here would otherwise report against
            // Foundation and every library — findings the reader cannot act on
            // because they do not own the code.
            //
            // Scoping covers findings only. An *error* is kept wherever it was
            // raised, because an error outside the linted path is not a finding
            // the reader cannot act on — it is the run failing. A lint whose own
            // runner would not evaluate reported nothing for exactly this
            // reason, under a printed `ok`, which is the shape of a fake
            // success: silence read as a clean bill of health.
            let owned: Vec<Diagnostic> = compiled
                .diagnostics
                .iter()
                .filter(|diagnostic| {
                    diagnostic.severity == kira_diagnostics::Severity::Error
                        // The receipt is the runner talking about itself, so it
                        // is anchored in the runner — outside the linted path,
                        // every time. Scoping it away is what made a run that
                        // never happened look like a run that found nothing.
                        || diagnostic.has_code(RECEIPT)
                        || under(path, diagnostic, &compiled.sources)
                })
                .cloned()
                .collect();
            // The runner's receipt, taken out of the findings before anything is
            // printed: it says how many lints ran, which is not something a
            // reader wants listed as a finding.
            let ran = lints_that_ran(&owned);
            let owned: Vec<Diagnostic> = owned
                .into_iter()
                .filter(|diagnostic| !diagnostic.has_code(RECEIPT))
                .collect();
            crate::diagnostics::emit_every(&owned, &compiled.sources);
            if kira_diagnostics::has_errors(&owned) {
                return EXIT_FAILURE;
            }
            let reported = owned.len();
            if apply {
                match apply_fixes(&owned, &compiled.sources) {
                    Ok(0) => out!("ok: {path} — nothing to fix"),
                    Ok(count) => {
                        out!("ok: {path} — applied {count} fix(es); run again to re-check")
                    }
                    Err(reason) => {
                        out!("kira lint: {reason}");
                        return EXIT_FAILURE;
                    }
                }
                return EXIT_OK;
            }
            // Silence is only good news when something was listening. Without
            // the receipt the runner did not run — no `linter.kira`, or one that
            // failed before it could report — and saying "clean" would be a
            // lie, so this fails instead.
            match ran {
                None => {
                    out!(
                        "kira lint: {path} — the lint runner did not run, so nothing was checked. \
                         Add a `linter.kira` beside `package.kira`, or read the errors above."
                    );
                    EXIT_FAILURE
                }
                // The runner ran and had nothing to run. Worded without naming a
                // file, because this is equally what a package with no
                // `linter.kira` gets and what one whose entries are all
                // `enabled = false` gets — and claiming a file exists that
                // does not is the same class of lie as claiming a clean run.
                Some(0) => {
                    out!(
                        "ok: {path} — no lint is enabled, so nothing was checked. \
                         Enable one in `linter.kira` beside `package.kira`."
                    );
                    EXIT_OK
                }
                Some(count) if reported == 0 => {
                    out!("ok: {path} — {count} lint(s) ran, nothing found");
                    EXIT_OK
                }
                Some(count) => {
                    out!("ok: {path} — {reported} report(s) from {count} lint(s)");
                    EXIT_OK
                }
            }
        }
        Err(code) => code,
    }
}

/// The code Foundation's lint runner reports its own arrival under.
///
/// Not a finding: it is how the runner says it ran, and how many lints it ran,
/// so silence can be told from absence. `kira lint` consumes it.
const RECEIPT: &str = "KLINT000";

/// How many lints ran, or `None` when the runner never reported.
///
/// The count is the trailing number of `lints ran: N`. A receipt that cannot be
/// read counts as no receipt: a run that cannot say what it checked has not
/// earned the word "clean".
fn lints_that_ran(diagnostics: &[Diagnostic]) -> Option<usize> {
    let receipt = diagnostics
        .iter()
        .find(|diagnostic| diagnostic.has_code(RECEIPT))?;
    // `{"lintsRan":N}`. Read by hand rather than through a JSON crate: it is one
    // field the runner and this function agree on, and the agreement is pinned
    // by a test either way.
    let count = receipt.message.split_once(':')?.1;
    count.trim_end_matches('}').trim().parse().ok()
}

/// Whether a diagnostic points inside the directory being linted.
///
/// Compared by canonical path so a relative `.` and an absolute root name the
/// same tree. A diagnostic with no span belongs to nobody in particular and is
/// kept, because dropping it would hide a whole-program complaint.
fn under(root: &str, diagnostic: &Diagnostic, sources: &SourceMap) -> bool {
    let Some(span) = diagnostic.primary_span() else {
        return true;
    };
    let index = span.source.value() as usize;
    if index >= sources.len() {
        return true;
    }
    let file = std::path::Path::new(&sources.get(span.source).path).to_path_buf();
    let root = std::path::Path::new(root);
    match (file.canonicalize(), root.canonicalize()) {
        (Ok(file), Ok(root)) => file.starts_with(root),
        // An unreadable path cannot be placed, and guessing would either hide a
        // real finding or invent one.
        _ => true,
    }
}

/// Writes every machine-applicable suggestion back to its file.
///
/// Back to front within each file, so an earlier edit never moves the span a
/// later one was measured against. Only `MachineApplicable` is written: anything
/// less is a suggestion for a reader, and applying it unattended is how a tool
/// silently changes what a program means.
fn apply_fixes(diagnostics: &[Diagnostic], sources: &SourceMap) -> Result<usize, String> {
    let mut per_file: std::collections::BTreeMap<usize, Vec<&Suggestion>> =
        std::collections::BTreeMap::new();
    for diagnostic in diagnostics {
        let Some(suggestion) = &diagnostic.suggestion else {
            continue;
        };
        if !suggestion.is_machine_applicable() {
            continue;
        }
        per_file
            .entry(suggestion.span.source.value() as usize)
            .or_default()
            .push(suggestion);
    }

    let mut applied = 0;
    for (index, mut fixes) in per_file {
        if index >= sources.len() {
            continue;
        }
        let file = sources.get(SourceId::new(index as u32));
        let mut text = file.text.clone();
        // Descending by start, so each write leaves every earlier span intact.
        fixes.sort_by_key(|fix| std::cmp::Reverse(fix.span.span.start));
        for fix in fixes {
            let start = fix.span.span.start as usize;
            let end = fix.span.span.end() as usize;
            if end > text.len() || start > end {
                return Err(format!(
                    "a fix for `{}` names bytes {start}..{end}, which the file does not have",
                    file.path
                ));
            }
            text.replace_range(start..end, &fix.replacement);
            applied += 1;
        }
        std::fs::write(&file.path, text)
            .map_err(|error| format!("`{}` could not be written: {error}", file.path))?;
    }
    Ok(applied)
}
