//! The concrete syntax tree the parser produces.
//!
//! The tree follows the index/arena law: expressions and statements live in
//! arenas and reference each other by [`la_arena::Idx`], so no node carries a
//! lifetime. Every node records a [`Span`](kira_source::Span). The tree is
//! error-resilient — unparseable positions become [`Expr::Error`] /
//! [`Stmt::Error`] / an [`Item::Unsupported`] node rather than aborting the
//! parse.
//!
//! The nodes split across four cohesive modules — [`item`] for top-level
//! declarations and the type references they name, [`traits`] for trait
//! declarations and the conformance clause every declaration form shares,
//! [`stmt`] for statements and their sub-pieces, and [`expr`] for expressions
//! and operators — plus [`tree`] for the whole-file container. Every type is
//! re-exported flat here, so a consumer names `kira_syntax_model::ast::Expr`,
//! never the submodule.

mod expr;
mod item;
mod stmt;
mod traits;
mod tree;

pub use expr::{BinaryOp, CallArg, ClosureParam, Expr, FieldInit, TrailingClosure, UnaryOp};
pub use item::{
    ClassDecl, ClassMethod, ConstantDecl, ConstructDecl, ConstructField, ConstructKind,
    ConstructMethod, ConstructParent, DeferredConstruct, EnumDecl, ExportMark, ExtendDecl,
    FfiTypeKind, FfiTypeMark, FieldDecl, ForeignField, ForeignKind, ForeignMark, Function,
    ImportDecl, Item, OverrideFieldDecl, Param, ParentRef, StructDecl, TypeAliasDecl,
    TypeParamDecl, TypeRef, UnsupportedItem, VariantDecl,
};
pub use stmt::{Block, ForIterable, MatchArm, MatchBinding, Stmt};
pub use traits::{ReceiverDecl, TraitDecl, TraitMember, TraitRef};
pub use tree::{ExprId, FileNodes, FilePart, NodeBase, StmtId, SyntaxTree, TypeRefId};
