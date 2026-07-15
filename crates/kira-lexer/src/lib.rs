//! Tokenizer for Kira source text.
//!
//! Layer 1 of the Kira package graph.
//! Ported from kira-zig `packages/kira_lexer`.

pub mod lexer;

pub use lexer::{Lexer, tokenize};
