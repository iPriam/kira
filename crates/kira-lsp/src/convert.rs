//! Translating this compiler's diagnostics into the protocol's.
//!
//! # Positions are UTF-16, and Kira's spans are bytes
//!
//! LSP measures a position's `character` in **UTF-16 code units** by default,
//! counted from the start of the line. Kira's `Span` is a byte range. Those
//! agree for ASCII and disagree the moment a program contains a non-ASCII
//! character — an emoji in a string literal is one byte offset, two UTF-16
//! units, and four bytes. Squiggles land on the wrong column from there on.
//!
//! So the conversion counts UTF-16 units over the line prefix rather than
//! reusing the byte column `LineMap` already computes for the terminal
//! renderer, which measures what a terminal wants.
//!
//! Declaring `positionEncoding: utf-8` at initialize would sidestep this, but
//! it is a 3.17 capability a client may decline, and the fallback would still
//! have to exist. Doing it right unconditionally is less code than doing it
//! both ways.

use kira_diagnostics::{Diagnostic, Severity};
use kira_source::{SourceFile, Span};
use lsp_types::{
    Diagnostic as LspDiagnostic, DiagnosticRelatedInformation, DiagnosticSeverity, Location,
    NumberOrString, Position, Range, Uri,
};

use crate::analysis::DOCUMENT_SOURCE;

/// What the `source` field of every diagnostic this server publishes says.
const SOURCE: &str = "kira";

/// Converts one Kira diagnostic into the protocol's, anchored in `file`.
///
/// The primary label is the squiggle; every other label becomes related
/// information, so a multi-span diagnostic keeps the secondary sites a reader
/// needs instead of flattening to one.
pub fn diagnostic(diagnostic: &Diagnostic, file: &SourceFile, uri: &Uri) -> LspDiagnostic {
    let primary = diagnostic
        .labels
        .iter()
        .find(|label| label.span.source == DOCUMENT_SOURCE)
        .map(|label| range(file, label.span.span))
        // A diagnostic about the program as a whole (no `@Main`, say) has no
        // site to point at. Anchor it at the start rather than dropping it: an
        // unplaceable error is still an error the user must see.
        .unwrap_or_default();

    let related: Vec<DiagnosticRelatedInformation> = diagnostic
        .labels
        .iter()
        .skip(1)
        .filter(|label| label.span.source == DOCUMENT_SOURCE)
        .map(|label| DiagnosticRelatedInformation {
            location: Location {
                uri: uri.clone(),
                range: range(file, label.span.span),
            },
            message: label.message.clone(),
        })
        .collect();

    LspDiagnostic {
        range: primary,
        severity: Some(severity(diagnostic.severity)),
        code: diagnostic
            .code
            .as_ref()
            .map(|code| NumberOrString::String(code.as_str().to_owned())),
        code_description: None,
        source: Some(SOURCE.to_owned()),
        message: message(diagnostic),
        related_information: (!related.is_empty()).then_some(related),
        tags: None,
        data: None,
    }
}

/// The text an editor shows for a diagnostic.
///
/// The title is the one-line summary and the message is the explanation; the
/// terminal renderer shows both, so this does too. Notes follow, because they
/// routinely carry the actual fix.
fn message(diagnostic: &Diagnostic) -> String {
    let mut out = diagnostic.title.clone();
    if !diagnostic.message.is_empty() && diagnostic.message != diagnostic.title {
        out.push('\n');
        out.push_str(&diagnostic.message);
    }
    for note in &diagnostic.notes {
        out.push_str("\n\nnote: ");
        out.push_str(note);
    }
    out
}

/// Maps Kira's severity onto the protocol's.
fn severity(severity: Severity) -> DiagnosticSeverity {
    match severity {
        Severity::Error => DiagnosticSeverity::ERROR,
        Severity::Warning => DiagnosticSeverity::WARNING,
        Severity::Note => DiagnosticSeverity::INFORMATION,
    }
}

/// Converts a byte span into a UTF-16 line/character range.
pub fn range(file: &SourceFile, span: Span) -> Range {
    Range {
        start: position(file, span.start),
        end: position(file, span.start.saturating_add(span.len)),
    }
}

/// Converts a byte offset into a UTF-16 line/character position.
///
/// Total by construction: an offset past the end, or one landing inside a
/// multi-byte character, is clamped to the nearest boundary at or before it. A
/// language server that panicked on a span would take the editor's whole
/// experience down with it, so there is no offset this rejects.
fn position(file: &SourceFile, offset: u32) -> Position {
    let offset = boundary(&file.text, offset as usize);
    let line_index = file.line_map.line_index(offset as u32);
    let (line_start, _) = file.line_map.line_bounds(line_index, &file.text);
    let prefix = &file.text[boundary(&file.text, line_start as usize)..offset];
    Position {
        // `LineMap` counts lines from 1 for humans; LSP counts from 0.
        line: line_index as u32,
        character: prefix.encode_utf16().count() as u32,
    }
}

/// Converts a UTF-16 line/character position into a byte offset.
///
/// The inverse of [`range`]'s conversion, and total the same way: a line past
/// the end clamps to the end of the text, and a character past the end of its
/// line clamps to the end of that line — a client is entitled to send a
/// position one keystroke stale, and the worst it may get back is the nearest
/// boundary, never a panic.
pub fn offset(file: &SourceFile, position: Position) -> u32 {
    // `line_count` is at least one even for empty text, so the subtraction
    // cannot wrap.
    let line_index = (position.line as usize).min(file.line_map.line_count() - 1);
    let (line_start, line_end) = file.line_map.line_bounds(line_index, &file.text);
    let line = &file.text[line_start as usize..line_end as usize];
    let mut units = 0u32;
    for (byte, character) in line.char_indices() {
        if units >= position.character {
            return line_start + byte as u32;
        }
        units += character.len_utf16() as u32;
    }
    line_end
}

/// The largest char boundary at or before `offset`, clamped into `text`.
fn boundary(text: &str, offset: usize) -> usize {
    let mut offset = offset.min(text.len());
    while offset > 0 && !text.is_char_boundary(offset) {
        offset -= 1;
    }
    offset
}

#[cfg(test)]
mod tests {
    use super::*;
    use kira_source::SourceFile;

    fn file(text: &str) -> SourceFile {
        SourceFile::new(DOCUMENT_SOURCE, "t.kira".to_owned(), text.to_owned())
    }

    #[test]
    fn a_span_on_the_first_line_maps_from_zero() {
        let file = file("let x = 1\nlet y = 2\n");
        // `x` is at byte 4 on line 0.
        let range = range(&file, Span { start: 4, len: 1 });
        assert_eq!(
            range.start,
            Position {
                line: 0,
                character: 4
            }
        );
        assert_eq!(
            range.end,
            Position {
                line: 0,
                character: 5
            }
        );
    }

    #[test]
    fn a_span_on_a_later_line_is_relative_to_that_line() {
        let file = file("let x = 1\nlet y = 2\n");
        // `y` is at byte 14, which is column 4 of line 1.
        let range = range(&file, Span { start: 14, len: 1 });
        assert_eq!(
            range.start,
            Position {
                line: 1,
                character: 4
            }
        );
    }

    /// The bug this module exists to avoid: a non-ASCII character earlier on
    /// the line makes the byte column and the UTF-16 column disagree, and a
    /// span measured in bytes lands on the wrong column from there on.
    #[test]
    fn a_position_after_a_multi_byte_character_counts_utf16_not_bytes() {
        // `é` is 2 bytes but 1 UTF-16 unit, so every offset after it is one
        // greater in bytes than in characters.
        let text = "let é = x";
        let file = file(text);
        let byte = text.find('x').expect("the name is there") as u32;
        assert_eq!(byte, 9, "`x` sits at byte 9");

        let position = position(&file, byte);
        assert_eq!(position.line, 0);
        assert_eq!(
            position.character, 8,
            "byte 9 is UTF-16 unit 8: `é` counts once, not twice",
        );
    }

    /// An emoji is 4 bytes but *two* UTF-16 units (a surrogate pair) — the case
    /// that also breaks a naive `chars().count()` conversion.
    #[test]
    fn a_surrogate_pair_counts_as_two_utf16_units() {
        let text = "\"🔥\" + x";
        let file = file(text);
        let after = text.find('+').expect("the operator is there") as u32;
        let position = position(&file, after);
        // `"` + emoji(2 units) + `"` + space = 5 units before `+`.
        assert_eq!(position.character, 5);
    }

    /// A language server may not panic on a span, however odd.
    #[test]
    fn an_out_of_range_or_mid_character_offset_is_clamped_rather_than_panicking() {
        let file = file("é");
        // Byte 1 is inside `é`; byte 99 is past the end.
        assert_eq!(position(&file, 1).character, 0);
        assert_eq!(position(&file, 99).character, 1);
    }

    /// `offset` inverts `position` on every boundary the protocol can send:
    /// ASCII, multi-byte, line ends, and positions past the text.
    #[test]
    fn an_lsp_position_maps_back_to_the_byte_it_came_from() {
        let file = file("let é = 1\nlet y = x\n");
        // Round trip: the byte of `y` survives position → offset.
        let y = 14u32;
        assert_eq!(offset(&file, position(&file, y)), y);
        // A character past the end of its line clamps to the line end, not
        // into the next line.
        assert_eq!(
            offset(
                &file,
                Position {
                    line: 0,
                    character: 99
                }
            ),
            10,
            "line 0 ends at byte 10 (`é` is two bytes)"
        );
        // A line past the end clamps to the last line.
        assert_eq!(
            offset(
                &file,
                Position {
                    line: 99,
                    character: 0
                }
            ),
            21
        );
    }

    #[test]
    fn severities_map_onto_the_protocols() {
        assert_eq!(severity(Severity::Error), DiagnosticSeverity::ERROR);
        assert_eq!(severity(Severity::Warning), DiagnosticSeverity::WARNING);
        assert_eq!(severity(Severity::Note), DiagnosticSeverity::INFORMATION);
    }
}
