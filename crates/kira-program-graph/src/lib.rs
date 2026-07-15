//! Whole-program module graph construction from parsed packages.
//!
//! Layer 6 of the Kira package graph.
//! Ported from kira-zig `packages/kira_program_graph`.
//!
//! Module map (one Rust module per Zig source file):
//! - [`builder`] — graph construction, per-package module collection, parse
//!   drivers, timings/progress hooks (`builder.zig`).
//! - [`imports`] — import path resolution against the package module map
//!   (`imports.zig`).
//! - [`paths`] — path canonicalization and existence helpers (`paths.zig`).
//! - [`roots`] — package-root -> `app/` source-root mapping (`roots.zig`).

pub mod builder;
pub mod imports;
pub mod paths;
pub mod roots;

pub use builder::ProgramGraph;
pub use imports::ImportResolution;
