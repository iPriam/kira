//! The typed shader IR tree: one `Program` per KSL compilation, holding
//! type declarations, free functions, and fully-resolved shader declarations
//! (with reflection) ready for backend lowering.
//!
//! Ported from kira-zig `packages/kira_shader_ir/src/ir.zig`.

use kira_shader_model as shader_model;

/// Source span (Zig: `kira_source.Span`), owned by `kira-source` — exactly
/// one `Span` definition in the workspace.
pub use kira_source::Span;

/// A whole analyzed KSL program. Zig: `Program`.
#[derive(Debug, Clone, PartialEq)]
pub struct Program {
    pub imported_modules: Vec<ImportedModule>,
    pub types: Vec<TypeDecl>,
    pub functions: Vec<FunctionDecl>,
    pub shaders: Vec<ShaderDecl>,
}

/// An imported KSL module reference. Zig: `ImportedModule`.
#[derive(Debug, Clone, PartialEq)]
pub struct ImportedModule {
    pub alias: String,
    pub module_name: String,
}

/// A user struct declaration with optional memory layouts. Zig: `TypeDecl`.
#[derive(Debug, Clone, PartialEq)]
pub struct TypeDecl {
    pub name: String,
    pub fields: Vec<FieldDecl>,
    pub uniform_layout: Option<StructLayout>,
    pub storage_layout: Option<StructLayout>,
    pub span: Span,
}

/// One struct field. Zig: `FieldDecl`.
#[derive(Debug, Clone, PartialEq)]
pub struct FieldDecl {
    pub name: String,
    pub ty: shader_model::Type,
    pub builtin: Option<shader_model::Builtin>,
    pub interpolation: Option<shader_model::Interpolation>,
    pub span: Span,
}

/// Computed memory layout of a struct for a binding class. Zig: `StructLayout`.
#[derive(Debug, Clone, PartialEq)]
pub struct StructLayout {
    pub alignment: u32,
    pub size: u32,
    pub fields: Vec<FieldLayout>,
}

/// Layout of one struct field. Zig: `FieldLayout`.
#[derive(Debug, Clone, PartialEq)]
pub struct FieldLayout {
    pub name: String,
    pub offset: u32,
    pub alignment: u32,
    pub size: u32,
    pub stride: u32,
}

/// A fully-analyzed shader declaration. Zig: `ShaderDecl`.
#[derive(Debug, Clone, PartialEq)]
pub struct ShaderDecl {
    pub name: String,
    pub kind: shader_model::ShaderKind,
    pub options: Vec<OptionDecl>,
    pub groups: Vec<GroupDecl>,
    pub stages: Vec<StageDecl>,
    pub reflection: shader_model::Reflection,
    pub span: Span,
}

/// A compile-time option with its default. Zig: `OptionDecl`.
#[derive(Debug, Clone, PartialEq)]
pub struct OptionDecl {
    pub name: String,
    pub ty: shader_model::Type,
    pub default_value: ConstValue,
    pub span: Span,
}

/// A resource group declaration. Zig: `GroupDecl`.
#[derive(Debug, Clone, PartialEq)]
pub struct GroupDecl {
    pub name: String,
    pub class: shader_model::GroupClass,
    pub resources: Vec<ResourceDecl>,
    pub span: Span,
}

/// A resource declaration with logical binding indices. Zig: `ResourceDecl`.
#[derive(Debug, Clone, PartialEq)]
pub struct ResourceDecl {
    pub name: String,
    pub kind: shader_model::ResourceKind,
    pub access: Option<shader_model::AccessMode>,
    pub ty: shader_model::Type,
    pub visibility: Vec<shader_model::Stage>,
    pub logical_group_index: u32,
    pub logical_binding_index: u32,
    pub span: Span,
}

/// Compute workgroup dimensions. Zig: `Threads`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Threads {
    pub x: u32,
    pub y: u32,
    pub z: u32,
}

/// One stage of a shader with its entry function. Zig: `StageDecl`.
#[derive(Debug, Clone, PartialEq)]
pub struct StageDecl {
    pub kind: shader_model::Stage,
    pub input_type: Option<String>,
    pub output_type: Option<String>,
    pub threads: Option<Threads>,
    pub entry: FunctionDecl,
    pub span: Span,
}

/// A function declaration (free function or stage entry). Zig: `FunctionDecl`.
#[derive(Debug, Clone, PartialEq)]
pub struct FunctionDecl {
    pub name: String,
    pub params: Vec<ParamDecl>,
    pub return_type: shader_model::Type,
    pub body: Block,
    pub module_alias: Option<String>,
    pub span: Span,
}

/// One function parameter. Zig: `ParamDecl`.
#[derive(Debug, Clone, PartialEq)]
pub struct ParamDecl {
    pub name: String,
    pub ty: shader_model::Type,
    pub span: Span,
}

/// A statement block. Zig: `Block`.
#[derive(Debug, Clone, PartialEq)]
pub struct Block {
    pub statements: Vec<Statement>,
    pub span: Span,
}

/// A statement. Zig: `Statement` (tagged union).
#[derive(Debug, Clone, PartialEq)]
pub enum Statement {
    Let(LetStatement),
    Assign(AssignStatement),
    Expr(ExprStatement),
    Return(ReturnStatement),
    If(IfStatement),
    While(WhileStatement),
}

/// `while` loop. Zig: `WhileStatement`.
#[derive(Debug, Clone, PartialEq)]
pub struct WhileStatement {
    pub condition: Box<Expr>,
    pub body: Block,
    pub span: Span,
}

/// `let` binding. Zig: `LetStatement`.
#[derive(Debug, Clone, PartialEq)]
pub struct LetStatement {
    pub name: String,
    pub ty: shader_model::Type,
    pub value: Option<Box<Expr>>,
    pub span: Span,
}

/// Assignment. Zig: `AssignStatement`.
#[derive(Debug, Clone, PartialEq)]
pub struct AssignStatement {
    pub target: Box<Expr>,
    pub value: Box<Expr>,
    pub span: Span,
}

/// Expression statement. Zig: `ExprStatement`.
#[derive(Debug, Clone, PartialEq)]
pub struct ExprStatement {
    pub expr: Box<Expr>,
    pub span: Span,
}

/// `return`. Zig: `ReturnStatement`.
#[derive(Debug, Clone, PartialEq)]
pub struct ReturnStatement {
    pub value: Option<Box<Expr>>,
    pub span: Span,
}

/// `if`/`else`. Zig: `IfStatement`.
#[derive(Debug, Clone, PartialEq)]
pub struct IfStatement {
    pub condition: Box<Expr>,
    pub then_block: Block,
    pub else_block: Option<Block>,
    pub span: Span,
}

/// A typed expression. Zig: `Expr` (`ty`, `span`, `node`).
#[derive(Debug, Clone, PartialEq)]
pub struct Expr {
    pub ty: shader_model::Type,
    pub span: Span,
    pub node: ExprNode,
}

/// Expression payload. Zig: `Expr.Node` (tagged union).
#[derive(Debug, Clone, PartialEq)]
pub enum ExprNode {
    ConstValue(ConstValue),
    Name(NameRef),
    Unary(UnaryExpr),
    Binary(BinaryExpr),
    Call(CallExpr),
    Member(MemberExpr),
    Index(IndexExpr),
}

/// What a name resolves to. Zig: `NameKind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NameKind {
    Local,
    Param,
    Option,
    Resource,
    Function,
    ImportedFunction,
}

/// A resolved name reference. Zig: `NameRef`.
#[derive(Debug, Clone, PartialEq)]
pub struct NameRef {
    pub kind: NameKind,
    pub name: String,
    pub module_alias: Option<String>,
}

/// Unary expression. Zig: `UnaryExpr`.
#[derive(Debug, Clone, PartialEq)]
pub struct UnaryExpr {
    pub op: UnaryOp,
    pub operand: Box<Expr>,
}

/// Unary operator. Zig: `UnaryOp`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UnaryOp {
    Neg,
    Not,
}

/// Binary expression. Zig: `BinaryExpr`.
#[derive(Debug, Clone, PartialEq)]
pub struct BinaryExpr {
    pub op: BinaryOp,
    pub left: Box<Expr>,
    pub right: Box<Expr>,
}

/// Binary operator. Zig: `BinaryOp`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BinaryOp {
    Add,
    Sub,
    Mul,
    Div,
    Less,
    LessEqual,
    Greater,
    GreaterEqual,
    Equal,
    NotEqual,
}

/// Call expression. Zig: `CallExpr`.
#[derive(Debug, Clone, PartialEq)]
pub struct CallExpr {
    pub callee: Callee,
    pub args: Vec<Expr>,
}

/// What a call targets. Zig: `Callee` (tagged union).
#[derive(Debug, Clone, PartialEq)]
pub enum Callee {
    Function(NameRef),
    Constructor(shader_model::Type),
    Intrinsic(Intrinsic),
}

/// Backend-lowered intrinsic function. Zig: `Intrinsic`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Intrinsic {
    Mul,
    Normalize,
    Dot,
    Sample,
    Load,
    AtomicAdd,
    Length,
    Pow,
    Sin,
    Atan2,
    Smoothstep,
}

/// Member access. Zig: `MemberExpr`.
#[derive(Debug, Clone, PartialEq)]
pub struct MemberExpr {
    pub object: Box<Expr>,
    pub name: String,
}

/// Index access. Zig: `IndexExpr`.
#[derive(Debug, Clone, PartialEq)]
pub struct IndexExpr {
    pub object: Box<Expr>,
    pub index: Box<Expr>,
}

/// A compile-time constant. Zig: `ConstValue` (tagged union).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ConstValue {
    Bool(bool),
    Int(i32),
    Uint(u32),
    Float(f32),
}
