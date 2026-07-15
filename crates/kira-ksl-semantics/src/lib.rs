//! Semantic analysis for KSL modules, producing shader IR.
//!
//! Layer 2 of the Kira package graph.
//! Ported from kira-zig `packages/kira_ksl_semantics` (`analyzer.zig`,
//! `analyzer_utils.zig`, `function_scope.zig`).

// #![warn(missing_docs)] // enable once the port lands real code

pub mod analyzer;
pub mod analyzer_utils;
pub mod function_scope;

pub use analyzer::{Analyzer, ImportedModule};

/// Returns this crate's name (scaffold smoke check).
pub fn crate_name() -> &'static str {
    "kira-ksl-semantics"
}
