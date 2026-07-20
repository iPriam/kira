//! Offset-to-line/column translation for one source file.

/// A 1-based line/column position inside a source file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LineColumn {
    /// 1-based line number.
    pub line: u32,
    /// 1-based column number (byte-based).
    pub column: u32,
}

/// Precomputed newline offsets enabling O(log n) offset-to-position lookups.
#[derive(Debug, Clone, Default)]
pub struct LineMap {
    /// Byte offset of the first byte of each line; always starts with 0.
    line_starts: Vec<u32>,
}

impl LineMap {
    /// Scans `text` once and records where every line starts.
    pub fn new(text: &str) -> Self {
        let mut line_starts = vec![0];
        for (index, byte) in text.bytes().enumerate() {
            if byte == b'\n' {
                line_starts.push(index as u32 + 1);
            }
        }
        Self { line_starts }
    }

    /// Returns the 1-based line/column of a byte offset.
    pub fn line_column(&self, offset: u32) -> LineColumn {
        let line_index = self.line_index(offset);
        let line_start = self.line_starts[line_index];
        LineColumn {
            line: line_index as u32 + 1,
            column: offset - line_start + 1,
        }
    }

    /// How many lines the file has; at least one, even for empty text.
    pub fn line_count(&self) -> usize {
        self.line_starts.len()
    }

    /// Returns the 0-based index of the line containing a byte offset.
    pub fn line_index(&self, offset: u32) -> usize {
        self.line_starts.partition_point(|&start| start <= offset) - 1
    }

    /// Returns the `[start, end)` byte bounds of a 0-based line, excluding its newline.
    pub fn line_bounds(&self, line_index: usize, text: &str) -> (u32, u32) {
        let start = self.line_starts[line_index];
        let raw_end = self
            .line_starts
            .get(line_index + 1)
            .copied()
            .unwrap_or(text.len() as u32);
        let end = if raw_end > start && text.as_bytes()[raw_end as usize - 1] == b'\n' {
            raw_end - 1
        } else {
            raw_end
        };
        (start, end)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_offsets_to_lines_and_columns() {
        let map = LineMap::new("one\ntwo\nthree");
        assert_eq!(map.line_column(0), LineColumn { line: 1, column: 1 });
        assert_eq!(map.line_column(4), LineColumn { line: 2, column: 1 });
        assert_eq!(map.line_column(6), LineColumn { line: 2, column: 3 });
        assert_eq!(map.line_bounds(1, "one\ntwo\nthree"), (4, 7));
    }
}
