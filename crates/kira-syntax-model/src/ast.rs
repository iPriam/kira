//! The concrete syntax tree the parser produces.
//!
//! The tree follows the index/arena law: expressions and statements live in
//! arenas and reference each other by [`la_arena::Idx`], so no node carries a
//! lifetime. Every node records a [`Span`](kira_source::Span). The tree is
//! error-resilient — unparseable positions become [`Expr::Error`] /
//! [`Stmt::Error`] / an [`Item::Unsupported`] node rather than aborting the
//! parse.
//!
//! The nodes split across three cohesive modules — [`item`] for top-level
//! declarations and the type references they name, [`stmt`] for statements and
//! their sub-pieces, and [`expr`] for expressions and operators — plus [`tree`]
//! for the whole-file container. Every type is re-exported flat here, so a
//! consumer names `kira_syntax_model::ast::Expr`, never the submodule.

mod expr;
mod item;
mod stmt;
mod tree;

pub use expr::{BinaryOp, ClosureParam, Expr, FieldInit, UnaryOp};
pub use item::{
    ClassDecl, ClassMethod, EnumDecl, ExportMark, FieldDecl, ForeignField, ForeignMark, Function,
    ImportDecl, Item, OverrideFieldDecl, Param, ParentRef, StructDecl, TypeAliasDecl,
    TypeParamDecl, TypeRef, UnsupportedItem, VariantDecl,
};
pub use stmt::{Block, ForIterable, MatchArm, MatchBinding, Stmt, SwitchCase};
pub use tree::{ExprId, StmtId, SyntaxTree, TypeRefId};
