//! Human-readable diagnostic rendering (source excerpts, carets, colors).
//!
//! Mirrors kira-zig `packages/kira_diagnostics/src/renderer.zig`.
//! TODO(port): the full renderer (line excerpts, label underlining, note and
//! help formatting) lands during migration; this is a placeholder surface.

use crate::diagnostic::Diagnostic;
use kira_source::SourceMap;

/// Renders one diagnostic against its sources.
///
/// TODO(port): currently returns an empty string; the real renderer is ported
/// from `renderer.zig` during migration.
pub fn render(_diagnostic: &Diagnostic, _sources: &SourceMap) -> String {
    String::new()
}
