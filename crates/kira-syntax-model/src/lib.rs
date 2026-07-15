//! Kira syntax model: tokens, syntax kinds, and the AST.
//!
//! Layer 1 of the Kira package graph.
//! Ported from kira-zig `packages/kira_syntax_model`.

pub mod ast;
pub mod ast_dump;
pub mod ast_exprs;
pub mod syntax_kinds;
pub mod token;

pub use syntax_kinds::SyntaxKind;
pub use token::{Token, TokenKind};
