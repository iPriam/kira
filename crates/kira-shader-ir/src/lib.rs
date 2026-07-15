//! Shader IR lowered from KSL, consumed by the shader-language backends.
//!
//! Layer 3 of the Kira package graph.
//! Ported from kira-zig `packages/kira_shader_ir` (`ir.zig`).

// #![warn(missing_docs)] // enable once the port lands real code

pub mod ir;

pub use ir::{
    AssignStatement, BinaryExpr, BinaryOp, Block, CallExpr, Callee, ConstValue, Expr, ExprNode,
    ExprStatement, FieldDecl, FieldLayout, FunctionDecl, GroupDecl, IfStatement, ImportedModule,
    IndexExpr, Intrinsic, LetStatement, MemberExpr, NameKind, NameRef, OptionDecl, ParamDecl,
    Program, ResourceDecl, ReturnStatement, ShaderDecl, Span, StageDecl, Statement, StructLayout,
    Threads, TypeDecl, UnaryExpr, UnaryOp, WhileStatement,
};

/// Returns this crate's name (scaffold smoke check).
pub fn crate_name() -> &'static str {
    "kira-shader-ir"
}
