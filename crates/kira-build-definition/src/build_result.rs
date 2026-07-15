//! Build results.
//!
//! Ported from kira-zig `kira_build_definition/src/build_result.zig`.

use crate::artifact::Artifact;

/// The artifacts a build produced (Zig `BuildResult`).
#[derive(Debug, Clone, Default)]
pub struct BuildResult {
    /// Zig `artifacts: []const Artifact`.
    pub artifacts: Vec<Artifact>,
}
