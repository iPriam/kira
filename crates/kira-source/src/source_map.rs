//! The collection of all source files known to one compilation.
//!
//! Rust-port addition: kira-zig threads `*const SourceFile` and a thread-local
//! default source path around; the Rust port instead resolves a [`SourceId`]
//! through this map.

use crate::source_file::SourceFile;
use crate::span::SourceId;

/// Owns every loaded [`SourceFile`] and hands out [`SourceId`]s for them.
#[derive(Debug, Default)]
pub struct SourceMap {
    files: Vec<SourceFile>,
}

impl SourceMap {
    /// Creates an empty source map.
    pub fn new() -> Self {
        Self::default()
    }

    /// Loads `text` under `path`, returning the id of the new file.
    pub fn insert(&mut self, path: String, text: String) -> SourceId {
        let id = SourceId::new(u32::try_from(self.files.len()).expect("source map overflow"));
        self.files.push(SourceFile::new(id, path, text));
        id
    }

    /// Returns the file behind an id issued by this map.
    pub fn get(&self, id: SourceId) -> &SourceFile {
        &self.files[id.value() as usize]
    }

    /// Number of files loaded so far.
    pub fn len(&self) -> usize {
        self.files.len()
    }

    /// True when no file has been loaded yet.
    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    /// Iterates over all loaded files in insertion order.
    pub fn iter(&self) -> impl Iterator<Item = &SourceFile> {
        self.files.iter()
    }
}
