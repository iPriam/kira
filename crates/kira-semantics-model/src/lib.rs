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
    HirPlaceStep, HirProgram, HirStmt, HirStmtId, HirUnaryOp, HirWriteback, LocalId,
};
/// The engine a function's body runs on, anchored in `kira-runtime-abi` and
/// re-exported here so the analyzer names it from one place.
pub use kira_runtime_abi::Execution;
/// The five ownership modes, anchored in the syntax model and re-exported here
/// so the analyzer and everything above it name them from one place.
pub use kira_syntax_model::ownership::{OwnershipMode, OwnershipOp};
pub use ty::{
    ArrayId, ArrayTable, CellId, CellTable, EnumDef, EnumId, EnumTable, ErasedTypeId, FieldDef,
    FloatSpelling, ForeignPtrId, ForeignPtrTable, Instantiation, IntSpelling, NativeStateId,
    NativeStateTable, StructDef, StructId, StructTable, TaskResult, Type, TypeTable, VariantDef,
};
