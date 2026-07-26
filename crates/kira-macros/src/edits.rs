//! The rewrite buffer: byte-range replacements over one file's text, plus the
//! declarations expansion appends to it.
//!
//! Every edit is expressed against the **original** offsets of the text it was
//! computed from, and they are applied in one left-to-right pass at the end, so
//! no edit ever has to be rebased on another. Overlapping edits are refused
//! rather than silently merged: two macros rewriting the same bytes is a bug in
//! this crate, not a program error, and dropping one would miscompile.

use kira_source::Span;

/// One byte-range replacement over a file's text.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Edit {
    /// Start byte offset, inclusive.
    start: u32,
    /// End byte offset, exclusive.
    end: u32,
    /// The text replacing that range.
    replacement: String,
}

/// Accumulated edits for one file.
#[derive(Debug, Default)]
pub(crate) struct EditBuffer {
    edits: Vec<Edit>,
    appended: String,
    changed: bool,
}

impl EditBuffer {
    /// Creates an empty buffer.
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Whether anything at all was recorded.
    pub(crate) fn is_empty(&self) -> bool {
        !self.changed
    }

    /// Replaces `span` with `replacement`.
    pub(crate) fn replace(&mut self, span: Span, replacement: impl Into<String>) {
        self.changed = true;
        self.edits.push(Edit {
            start: span.start,
            end: span.end(),
            replacement: replacement.into(),
        });
    }

    /// Inserts `text` immediately before byte `offset`.
    pub(crate) fn insert(&mut self, offset: u32, text: impl Into<String>) {
        self.changed = true;
        self.edits.push(Edit {
            start: offset,
            end: offset,
            replacement: text.into(),
        });
    }

    /// Replaces `span` with whitespace of the same shape.
    ///
    /// Blanking rather than deleting is what keeps every *other* byte in the
    /// file at the offset it started at, so a diagnostic about untouched code
    /// still points at the line it was written on. Newlines are preserved for
    /// the same reason: the line numbering after a removed macro declaration is
    /// unchanged.
    pub(crate) fn blank(&mut self, span: Span, text: &str) {
        let slice = slice_of(text, span);
        let blanked: String = slice
            .chars()
            .map(|ch| if ch == '\n' { '\n' } else { ' ' })
            .collect();
        self.replace(span, blanked);
    }

    /// Appends `text` after everything already appended to this file.
    ///
    /// Generated declarations land at the end of the file rather than beside
    /// the declaration that produced them, because appending is the only edit
    /// that cannot move a byte the user wrote.
    pub(crate) fn append(&mut self, text: &str) {
        self.changed = true;
        if !self.appended.ends_with('\n') && !self.appended.is_empty() {
            self.appended.push('\n');
        }
        self.appended.push_str(text);
    }

    /// Applies every edit to `text`, returning the rewritten file.
    ///
    /// Edits are sorted by start offset; an edit that would overlap the
    /// previous one is dropped and reported through the returned flag, which
    /// the caller turns into a compiler-internal diagnostic rather than a wrong
    /// answer.
    pub(crate) fn apply(mut self, text: &str) -> Applied {
        self.edits.sort_by_key(|edit| (edit.start, edit.end));
        let mut out = String::with_capacity(text.len() + self.appended.len());
        let mut cursor = 0usize;
        let mut overlapped = false;
        for edit in &self.edits {
            let start = (edit.start as usize).min(text.len());
            let end = (edit.end as usize).min(text.len()).max(start);
            if start < cursor {
                overlapped = true;
                continue;
            }
            out.push_str(text.get(cursor..start).unwrap_or(""));
            out.push_str(&edit.replacement);
            cursor = end;
        }
        out.push_str(text.get(cursor..).unwrap_or(""));
        if !self.appended.is_empty() {
            if !out.ends_with('\n') {
                out.push('\n');
            }
            out.push_str(&self.appended);
            out.push('\n');
        }
        Applied {
            text: out,
            overlapped,
        }
    }
}

/// The result of applying an [`EditBuffer`].
pub(crate) struct Applied {
    /// The rewritten file text.
    pub(crate) text: String,
    /// Whether two edits claimed the same bytes and one was dropped.
    pub(crate) overlapped: bool,
}

/// The text `span` covers, clamped to `text`.
pub(crate) fn slice_of(text: &str, span: Span) -> &str {
    let start = (span.start as usize).min(text.len());
    let end = (span.end() as usize).min(text.len()).max(start);
    text.get(start..end).unwrap_or("")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edits_apply_left_to_right() {
        let text = "abcdef";
        let mut buffer = EditBuffer::new();
        buffer.replace(Span::from_bounds(4, 6), "Z");
        buffer.replace(Span::from_bounds(0, 2), "X");
        assert_eq!(buffer.apply(text).text, "XcdZ");
    }

    #[test]
    fn blanking_keeps_offsets_and_line_breaks() {
        let text = "one\ntwo\nthree";
        let mut buffer = EditBuffer::new();
        buffer.blank(Span::from_bounds(0, 7), text);
        let applied = buffer.apply(text);
        assert_eq!(applied.text, "   \n   \nthree");
        assert_eq!(applied.text.len(), text.len());
    }

    #[test]
    fn appended_text_lands_after_the_file() {
        let text = "a";
        let mut buffer = EditBuffer::new();
        buffer.append("generated");
        assert_eq!(buffer.apply(text).text, "a\ngenerated\n");
    }

    #[test]
    fn an_overlapping_edit_is_reported_rather_than_merged() {
        let text = "abcdef";
        let mut buffer = EditBuffer::new();
        buffer.replace(Span::from_bounds(0, 4), "X");
        buffer.replace(Span::from_bounds(2, 6), "Y");
        let applied = buffer.apply(text);
        assert!(applied.overlapped);
        assert_eq!(applied.text, "Xef");
    }

    #[test]
    fn an_insertion_keeps_the_bytes_around_it() {
        let text = "return x";
        let mut buffer = EditBuffer::new();
        buffer.insert(0, "let t = 1\n");
        assert_eq!(buffer.apply(text).text, "let t = 1\nreturn x");
    }
}
