//! Parser producing the Kira AST from tokens.
//!
//! Layer 1 of the Kira package graph.
//! Ported from kira-zig `packages/kira_parser` — one Rust module per Zig
//! parser file. The Zig test files (`parser_tests.zig`, `parser_root_tests.zig`,
//! `parser_app_surface_tests.zig`) port as Rust `#[cfg(test)]` suites alongside
//! the logic during migration.

pub mod blocks;
pub mod decls;
pub mod decls_complex;
pub mod decls_rules;
pub mod expr_postfix;
pub mod failtest;
pub mod macros;
pub mod params;
pub mod parser;
pub mod statements;
pub mod types_exprs;

pub use parser::{MAX_EXPR_DEPTH, ParseResult, Parser, parse};
