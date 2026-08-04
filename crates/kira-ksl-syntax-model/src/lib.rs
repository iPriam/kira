//! KSL (Kira Shading Language) token set and syntax tree.
//!
//! Layer 1 of the Kira package graph.
//!
//! KSL is a small, deliberately un-general language: a file declares struct
//! types, constants, enums, free functions, and one shader made of
//! compile-time options, resource groups, and stages. There are no generics,
//! no pointers, and no recursion, and the only loop is `while` — so the tree
//! stays flat enough that every backend can walk it directly.
//!
//! A `const` and an `enum` are source-level only: checking folds every read of
//! one into its value, so nothing named reaches a backend and none of the four
//! dialects has to agree on how a module-scope constant is spelled.
//!
//! The tree carries what was *written*, never what it means. `Float4` and
//! `Lighting.SceneLighting` are both paths here; which is a builtin vector and
//! which a struct reached through an import alias is decided in
//! `kira-ksl-semantics`.

pub mod ast;
pub mod token;
pub mod tree;

pub use ast::{
    Access, BinaryOp, Block, ConstDecl, EnumDecl, EnumVariant, Expr, Field, Function, Group,
    Import, Item, OptionDecl, Param, Resource, ResourceKind, Shader, StageDecl, StageWord, Stmt,
    TypeDecl, TypeRef, UnaryOp,
};
pub use token::{Token, TokenKind, keyword};
pub use tree::{ExprId, KslTree, StmtId, TypeRefId};
