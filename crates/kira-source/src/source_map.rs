//! The collection of all source files known to one compilation.
//!
//! Files are addressed by [`SourceId`] and resolved through this map, so
//! spans and diagnostics stay free of borrows into file storage.

use crate::source_file::{MAX_SOURCE_FILE_BYTES, SourceFile};
use crate::span::SourceId;

/// A source map cannot hold another file: every [`SourceId`] is taken.
///
/// A [`SourceId`] is a `u32`, so a map holds at most `u32::MAX` files. Reaching
/// that is a typed refusal rather than a panic — this type is in every
/// consumer's cone, and a library does not get to end its caller's process.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error(
    "this compilation already holds {} source files, the most a SourceId can address",
    u32::MAX
)]
pub struct SourceMapFull;

/// A source file exceeds [`MAX_SOURCE_FILE_BYTES`].
///
/// Every offset a span carries is a `u32`, so admitting files at or beyond
/// 4 GiB would silently corrupt their offsets; the documented 16 MiB cap
/// exists to keep that unreachable. Refusing here turns an overflow waiting
/// to happen into a named error.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error(
    "source file is {} bytes, over the {}-byte limit",
    .len,
    MAX_SOURCE_FILE_BYTES
)]
pub struct FileTooLarge {
    /// The size of the refused file in bytes.
    pub len: usize,
}

/// Why a source map refused a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SourceMapInsertError {
    /// Every [`SourceId`] is taken.
    #[error(transparent)]
    Full(#[from] SourceMapFull),
    /// The file is over the per-file size limit.
    #[error(transparent)]
    TooLarge(#[from] FileTooLarge),
}

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
    pub fn insert(&mut self, path: String, text: String) -> Result<SourceId, SourceMapInsertError> {
        let len = text.len();
        if len > MAX_SOURCE_FILE_BYTES {
            return Err(FileTooLarge { len }.into());
        }
        let raw = u32::try_from(self.files.len()).map_err(|_| SourceMapFull)?;
        let id = SourceId::new(raw);
        self.files.push(SourceFile::new(id, path, text));
        Ok(id)
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
