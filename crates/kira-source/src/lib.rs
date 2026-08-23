//! Source text model: spans, line maps, and source files.
//!
//! Layer 0 of the Kira package graph.

pub mod line_map;
pub mod source_file;
pub mod source_map;
pub mod span;

pub use line_map::{LineColumn, LineMap};
pub use source_file::{MAX_SOURCE_FILE_BYTES, SourceFile};
pub use source_map::{FileTooLarge, SourceMap, SourceMapFull, SourceMapInsertError};
pub use span::{FileSpan, SourceId, Span};
