//! Lexer and parser for KSL source.
//!
//! Layer 1 of the Kira package graph.
//! Ported from kira-zig `packages/kira_ksl_parser`.

pub mod lexer;
pub mod parser;

pub use lexer::tokenize;
pub use parser::parse;
