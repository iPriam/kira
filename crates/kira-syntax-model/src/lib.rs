//! Kira syntax model: the token set and syntax tree the parser produces.
//!
//! Layer 1 of the Kira package graph.
//!
//! The lexer and parser share this vocabulary: [`Token`]/[`TokenKind`] for the
//! token stream and [`SyntaxTree`] for the parsed result. The tree is
//! error-resilient (it carries [`Expr::Error`] / [`Stmt::Error`] /
//! [`Item::Unsupported`] nodes) and every node records a span, so the language
//! server and the compiler consume one frontend. Model types follow the
//! index/arena pattern and carry no lifetimes.

pub mod ast;
pub mod ownership;
pub mod token;

pub use ast::{
    BinaryOp, Block, Expr, ExprId, FfiTypeKind, FfiTypeMark, FileNodes, FilePart, ForIterable,
    ForeignField, ForeignMark, Function, Item, NodeBase, Param, Stmt, StmtId, SyntaxTree, TypeRef,
    TypeRefId, UnaryOp, UnsupportedItem,
};
pub use ownership::{OwnershipMode, OwnershipOp};
pub use token::{Token, TokenKind};
