//! Source span placeholder.
//!
//! TODO(port): replace with a re-export of `kira_source::Span` once the
//! frontend crate defines it (it is still an empty skeleton at scaffold
//! time). Mirrors kira-zig `kira_source/src/span.zig`.

/// Byte range into a source file (Zig `Span`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Span {
    /// Zig `start: usize`.
    pub start: usize,
    /// Zig `end: usize`.
    pub end: usize,
    /// Zig `source_path: ?[]const u8` (owned here).
    pub source_path: Option<String>,
}

impl Span {
    /// Creates a span with no source path (Zig `Span.init` without the
    /// thread-local default path).
    pub fn new(start: usize, end: usize) -> Span {
        Span {
            start,
            end,
            source_path: None,
        }
    }
}
