//! Source spans, re-exported from `kira-source`.
//!
//! `kira-source` owns the span model — exactly one `Span` definition in the
//! workspace. The Zig side's `Span.source_path` thread-local is replaced by
//! [`FileSpan`] (span + explicit `SourceId`); HIR nodes that need the file
//! identity carry a `FileSpan`, the rest carry a bare `Span`.

pub use kira_source::{FileSpan, SourceId, Span};
