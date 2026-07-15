//! One loaded source file: path, text, and line map.
//!
//! Mirrors kira-zig `packages/kira_source/src/source_file.zig`.

use crate::line_map::LineMap;
use crate::span::SourceId;

/// Upper bound on a single Kira source file's size (16 MiB, as in kira-zig).
pub const MAX_SOURCE_FILE_BYTES: usize = 16 * 1024 * 1024;

/// One source file loaded into memory, owning its text and line map.
#[derive(Debug)]
pub struct SourceFile {
    /// Identity of this file inside its [`crate::SourceMap`].
    pub id: SourceId,
    /// Path the file was loaded from (or a synthetic name for inline sources).
    pub path: String,
    /// Full file contents.
    pub text: String,
    /// Line-start table for offset-to-position lookups.
    pub line_map: LineMap,
}

impl SourceFile {
    /// Builds a source file from already-loaded text.
    pub fn new(id: SourceId, path: String, text: String) -> Self {
        let line_map = LineMap::new(&text);
        Self {
            id,
            path,
            text,
            line_map,
        }
    }
}
