//! Semantic model (HIR): the fully typed, name-resolved form of a program.
//!
//! Layer 2 of the Kira package graph.
//!
//! The HIR is designed for a salsa-query frontend: model types carry no
//! lifetimes (indices into arenas, owned names), so analysis holds no hidden
//! global state and the language server and compiler share it. See [`hir`] for
//! the tree and [`ty`] for the v0 type lattice.

pub mod hir;
pub mod ty;

pub use hir::{
    Builtin, Callee, FuncId, HirBinaryOp, HirExpr, HirExprId, HirFunction, HirLocal, HirPlace,
    HirPlaceStep, HirProgram, HirStmt, HirStmtId, HirUnaryOp, LocalId,
};
/// The five ownership modes, anchored in the syntax model and re-exported here
/// so the analyzer and everything above it name them from one place.
pub use kira_syntax_model::ownership::{OwnershipMode, OwnershipOp};
pub use ty::{ArrayId, ArrayTable, FieldDef, StructDef, StructId, StructTable, Type, TypeTable};
