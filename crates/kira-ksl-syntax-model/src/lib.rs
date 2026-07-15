//! KSL (Kira Shading Language) tokens and AST.
//!
//! Layer 1 of the Kira package graph.
//! Ported from kira-zig `packages/kira_ksl_syntax_model`.

pub mod ast;
pub mod token;

pub use ast::Module;
pub use token::{Token, TokenKind};
