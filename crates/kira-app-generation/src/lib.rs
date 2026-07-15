//! Project and app template generation (kira new).
//!
//! Layer 8 of the Kira package graph.
//! Ported from kira-zig `packages/kira_app_generation`.

// #![warn(missing_docs)] // enable once the port lands real code

pub mod generator;
pub mod templates;

pub use generator::TemplateKind;
