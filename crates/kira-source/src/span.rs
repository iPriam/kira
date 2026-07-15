//! Byte spans and source-file identity.
//!
//! Mirrors kira-zig `packages/kira_source/src/span.zig`. The Zig `Span`
//! carries an optional `source_path` slice (set through a thread-local
//! default); the Rust port replaces that with the [`SourceId`]/[`FileSpan`]
//! pair so no model type holds a lifetime.

/// Identifies one source file inside a [`crate::SourceMap`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SourceId(u32);

impl SourceId {
    /// Wraps a raw source-map index as a typed id.
    pub fn new(value: u32) -> Self {
        Self(value)
    }

    /// Returns the raw source-map index behind this id.
    pub fn value(self) -> u32 {
        self.0
    }
}

/// A byte range into one source file's text, stored as offset + length.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Span {
    /// Byte offset of the first byte covered by the span.
    pub start: u32,
    /// Number of bytes covered.
    pub len: u32,
}

impl Span {
    /// Builds a span from its start offset and length.
    pub fn new(start: u32, len: u32) -> Self {
        Self { start, len }
    }

    /// Builds a span from half-open `[start, end)` byte offsets (the Zig `Span.init` shape).
    pub fn from_bounds(start: u32, end: u32) -> Self {
        debug_assert!(end >= start);
        Self {
            start,
            len: end - start,
        }
    }

    /// Byte offset one past the last byte covered by the span.
    pub fn end(self) -> u32 {
        self.start + self.len
    }

    /// True when the span covers no bytes.
    pub fn is_empty(self) -> bool {
        self.len == 0
    }

    /// Returns the text this span covers inside `text` (the Zig `Span.slice`).
    pub fn slice(self, text: &str) -> &str {
        &text[self.start as usize..self.end() as usize]
    }
}

/// A [`Span`] paired with the [`SourceId`] it belongs to; replaces Zig's `Span.source_path`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FileSpan {
    /// The file the span points into.
    pub source: SourceId,
    /// The byte range inside that file.
    pub span: Span,
}

impl FileSpan {
    /// Pairs a span with the file it points into.
    pub fn new(source: SourceId, span: Span) -> Self {
        Self { source, span }
    }
}
