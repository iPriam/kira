//! What a command prints about a compilation, and what it leaves out.
//!
//! # Why notes are not printed by default
//!
//! A note is not about the program's correctness. It records something the
//! compiler decided — that GLSL 330 cannot express a shader binding a storage
//! buffer, that a drifted `kira.lock` was rewritten — and it records it again on
//! every build, because nothing about it changes when the source does. A dozen
//! such lines above a real error is what makes people stop reading the output.
//!
//! So the default is to count them and say so in one line, and `--show-notes`
//! prints them. The count is not optional: output that silently dropped
//! diagnostics would be worse than the noise it removed, because a reader
//! cannot ask for what they do not know was withheld.
//!
//! Errors and warnings are never hidden. There is no flag for that, and there
//! should not be.

use std::sync::atomic::{AtomicBool, Ordering};

use kira_diagnostics::{Diagnostic, Severity, renderer};
use kira_source::SourceMap;

use crate::progress::err;

/// Whether notes are printed, for the rest of this command.
///
/// A process-level setting for the same reason the progress sink is one: it is
/// the terminal's policy, decided once by whoever owns it, and threading it
/// through every function between a verb and a diagnostic would put a parameter
/// on each of them to answer a question none of them asks.
static SHOW_NOTES: AtomicBool = AtomicBool::new(false);

/// Prints notes for the rest of this command when `show`.
pub fn show_notes(show: bool) {
    SHOW_NOTES.store(show, Ordering::Relaxed);
}

/// Whether notes are being printed.
fn notes_shown() -> bool {
    SHOW_NOTES.load(Ordering::Relaxed)
}

/// Renders every diagnostic to stderr in source order, holding notes back
/// unless `--show-notes` asked for them.
pub fn emit(diagnostics: &[Diagnostic], sources: &SourceMap) {
    if notes_shown() {
        emit_every(diagnostics, sources);
        return;
    }
    let (notes, reported): (Vec<&Diagnostic>, Vec<&Diagnostic>) = diagnostics
        .iter()
        .partition(|diagnostic| diagnostic.severity == Severity::Note);
    render(reported.into_iter(), sources);
    if notes.is_empty() {
        return;
    }
    let count = notes.len();
    let plural = if count == 1 { "note" } else { "notes" };
    err!(
        "{}",
        kira_toolchain::Paint::auto_stderr().dim(&format!(
            "{count} {plural} not shown; run again with `--show-notes` to read them"
        ))
    );
}

/// Renders every diagnostic, notes included.
///
/// What `kira lint` reports through: a lint's findings are whatever severity
/// its author gave them, and a runner that reported at note severity would
/// otherwise say nothing under a printed `ok`.
pub fn emit_every(diagnostics: &[Diagnostic], sources: &SourceMap) {
    render(diagnostics.iter(), sources);
}

/// Writes `diagnostics` to stderr with the status surface stood aside.
fn render<'a>(diagnostics: impl Iterator<Item = &'a Diagnostic>, sources: &SourceMap) {
    // The status surface redraws in place; a diagnostic printed underneath it
    // would interleave into half a status block, a note, and a block that
    // scrolled. It stands aside and redraws on the next phase. Suspended once
    // for the whole run rather than per line, which `err!` would also do
    // correctly but at one erase check per diagnostic.
    let _surface = kira_diagnostics::progress::suspended();
    for diagnostic in diagnostics {
        eprint!("{}", renderer::render(diagnostic, sources));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notes_are_held_back_until_they_are_asked_for() {
        // The default is the one a build gets without saying anything.
        assert!(!notes_shown());
        show_notes(true);
        assert!(notes_shown());
        show_notes(false);
        assert!(!notes_shown());
    }
}
