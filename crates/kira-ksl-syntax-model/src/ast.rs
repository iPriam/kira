//! The KSL (Kira Shading Language) AST: modules, types, functions, shaders.
//!
//! Mirrors kira-zig `packages/kira_ksl_syntax_model/src/ast.zig`, translated
//! to the index/arena pattern: Zig `*Expr` / `*TypeRef` pointers become
//! [`ExprId`] / [`TypeRefId`] indices into [`AstArenas`]; name slices become
//! interned [`Symbol`]s; literal text stays owned `String`.

use kira_core::Symbol;
use kira_source::Span;
use la_arena::{Arena, Idx};

/// Arena index of an [`Expr`].
pub type ExprId = Idx<Expr>;
/// Arena index of a [`TypeRef`].
pub type TypeRefId = Idx<TypeRef>;

/// Arena storage for the pointer-allocated KSL node kinds.
#[derive(Debug, Default)]
pub struct AstArenas {
    /// All expressions of one parsed module.
    pub exprs: Arena<Expr>,
    /// All type references of one parsed module.
    pub type_refs: Arena<TypeRef>,
}

/// One parsed KSL source file, plus the arenas its nodes index into.
#[derive(Debug, Default)]
pub struct Module {
    /// Arena storage every node id of this module points into.
    pub arenas: AstArenas,
    /// The module's `import` declarations.
    pub imports: Vec<ImportDecl>,
    /// The module's `type` declarations.
    pub types: Vec<TypeDecl>,
    /// The module's free functions.
    pub functions: Vec<FunctionDecl>,
    /// The module's `shader` declarations.
    pub shaders: Vec<ShaderDecl>,
}

/// A dotted name path like `Math.Noise`.
#[derive(Debug, Clone, Default)]
pub struct QualifiedName {
    /// The segments in source order.
    pub segments: Vec<NameSegment>,
    /// Source range of the whole name.
    pub span: Span,
}

/// One segment of a dotted name, with its own span.
#[derive(Debug, Clone, Copy)]
pub struct NameSegment {
    /// The segment text.
    pub text: Symbol,
    /// Source range of the segment.
    pub span: Span,
}

/// An `import Module as Alias` declaration.
#[derive(Debug, Clone)]
pub struct ImportDecl {
    /// The imported module's dotted name.
    pub module_name: QualifiedName,
    /// The local alias, when written.
    pub alias: Option<Symbol>,
    /// Source range of the declaration.
    pub span: Span,
}

/// One applied annotation, e.g. `@location(0)`.
#[derive(Debug, Clone)]
pub struct Annotation {
    /// The annotation's name.
    pub name: QualifiedName,
    /// The annotation arguments in source order.
    pub args: Vec<ExprId>,
    /// Source range of the annotation.
    pub span: Span,
}

/// A reference to a type in KSL source.
#[derive(Debug, Clone)]
pub enum TypeRef {
    /// A named type, possibly qualified.
    Named(QualifiedName),
    /// A runtime-sized array type.
    RuntimeArray(RuntimeArrayType),
}

/// A runtime-sized array type over an element type.
#[derive(Debug, Clone)]
pub struct RuntimeArrayType {
    /// The element type.
    pub element: TypeRefId,
    /// Source range of the type.
    pub span: Span,
}

/// One field of a KSL `type` declaration.
#[derive(Debug, Clone)]
pub struct TypeField {
    /// Annotations preceding the field.
    pub annotations: Vec<Annotation>,
    /// The field name.
    pub name: Symbol,
    /// The field type.
    pub ty: TypeRefId,
    /// Source range of the field.
    pub span: Span,
}

/// A `type Name { fields }` declaration.
#[derive(Debug, Clone)]
pub struct TypeDecl {
    /// The type name.
    pub name: Symbol,
    /// The fields in source order.
    pub fields: Vec<TypeField>,
    /// Source range of the declaration.
    pub span: Span,
}

/// One parameter of a KSL function.
#[derive(Debug, Clone)]
pub struct ParamDecl {
    /// The parameter name.
    pub name: Symbol,
    /// The parameter type.
    pub ty: TypeRefId,
    /// Source range of the parameter.
    pub span: Span,
}

/// A `function name(params) -> Return { ... }` declaration.
#[derive(Debug, Clone)]
pub struct FunctionDecl {
    /// The function name.
    pub name: Symbol,
    /// The parameters in source order.
    pub params: Vec<ParamDecl>,
    /// The declared return type, when written.
    pub return_type: Option<TypeRefId>,
    /// The function body.
    pub body: Block,
    /// Source range of the declaration.
    pub span: Span,
}

/// A `shader Name { options, groups, stages }` declaration.
#[derive(Debug, Clone)]
pub struct ShaderDecl {
    /// The shader name.
    pub name: Symbol,
    /// The compile-time options in source order.
    pub options: Vec<OptionDecl>,
    /// The resource binding groups in source order.
    pub groups: Vec<GroupDecl>,
    /// The pipeline stages in source order.
    pub stages: Vec<StageDecl>,
    /// Source range of the declaration.
    pub span: Span,
}

/// An `option name: Type = default` compile-time shader option.
#[derive(Debug, Clone)]
pub struct OptionDecl {
    /// The option name.
    pub name: Symbol,
    /// The option type.
    pub ty: TypeRefId,
    /// The default value.
    pub default_value: ExprId,
    /// Source range of the declaration.
    pub span: Span,
}

/// A `group name { resources }` binding group.
#[derive(Debug, Clone)]
pub struct GroupDecl {
    /// The group name.
    pub name: Symbol,
    /// The resources in source order.
    pub resources: Vec<ResourceDecl>,
    /// Source range of the declaration.
    pub span: Span,
}

/// One resource binding inside a group.
#[derive(Debug, Clone)]
pub struct ResourceDecl {
    /// The resource kind.
    pub kind: ResourceKind,
    /// The access mode (storage resources).
    pub access: Option<AccessMode>,
    /// The resource name.
    pub name: Symbol,
    /// The resource type.
    pub ty: TypeRefId,
    /// Source range of the declaration.
    pub span: Span,
}

/// The KSL resource binding kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ResourceKind {
    /// A uniform buffer.
    Uniform,
    /// A storage buffer.
    Storage,
    /// A texture.
    Texture,
    /// A sampler.
    Sampler,
}

/// Storage resource access modes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AccessMode {
    /// Read-only access.
    Read,
    /// Read-write access.
    ReadWrite,
}

/// The pipeline stage kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StageKind {
    /// Vertex stage.
    Vertex,
    /// Fragment stage.
    Fragment,
    /// Compute stage.
    Compute,
}

/// One `vertex` / `fragment` / `compute` stage of a shader.
#[derive(Debug, Clone)]
pub struct StageDecl {
    /// The stage kind.
    pub kind: StageKind,
    /// The stage's input type, when declared.
    pub input_type: Option<QualifiedName>,
    /// The stage's output type, when declared.
    pub output_type: Option<QualifiedName>,
    /// The `threads(x, y, z)` workgroup size (compute stages).
    pub threads: Option<ThreadsDecl>,
    /// The stage entry function.
    pub entry: FunctionDecl,
    /// Source range of the stage.
    pub span: Span,
}

/// A `threads(x, y, z)` compute workgroup size declaration.
#[derive(Debug, Clone)]
pub struct ThreadsDecl {
    /// Workgroup size along x.
    pub x: ExprId,
    /// Workgroup size along y.
    pub y: ExprId,
    /// Workgroup size along z.
    pub z: ExprId,
    /// Source range of the declaration.
    pub span: Span,
}

/// A `{ ... }` statement block.
#[derive(Debug, Clone, Default)]
pub struct Block {
    /// The statements in source order.
    pub statements: Vec<Statement>,
    /// Source range of the block.
    pub span: Span,
}

/// One KSL statement.
#[derive(Debug, Clone)]
pub enum Statement {
    /// `let` binding.
    Let(LetStatement),
    /// Assignment to an lvalue.
    Assign(AssignStatement),
    /// A bare expression evaluated for effect.
    Expr(ExprStatement),
    /// `return` with optional value.
    Return(ReturnStatement),
    /// `if` / `else`.
    If(IfStatement),
    /// `while`.
    While(WhileStatement),
}

/// A `while condition { ... }` loop.
#[derive(Debug, Clone)]
pub struct WhileStatement {
    /// The loop condition.
    pub condition: ExprId,
    /// The loop body.
    pub body: Block,
    /// Source range of the statement.
    pub span: Span,
}

/// A `let name: Type = value` binding.
#[derive(Debug, Clone)]
pub struct LetStatement {
    /// The bound name.
    pub name: Symbol,
    /// The declared type, when written.
    pub ty: Option<TypeRefId>,
    /// The initializer, when written.
    pub value: Option<ExprId>,
    /// Source range of the statement.
    pub span: Span,
}

/// An assignment `target = value`.
#[derive(Debug, Clone)]
pub struct AssignStatement {
    /// The assignable place.
    pub target: ExprId,
    /// The new value.
    pub value: ExprId,
    /// Source range of the statement.
    pub span: Span,
}

/// A bare expression evaluated as a statement.
#[derive(Debug, Clone)]
pub struct ExprStatement {
    /// The expression.
    pub expr: ExprId,
    /// Source range of the statement.
    pub span: Span,
}

/// A `return` statement with optional value.
#[derive(Debug, Clone)]
pub struct ReturnStatement {
    /// The returned value, when written.
    pub value: Option<ExprId>,
    /// Source range of the statement.
    pub span: Span,
}

/// An `if condition { ... } else { ... }` statement.
#[derive(Debug, Clone)]
pub struct IfStatement {
    /// The condition expression.
    pub condition: ExprId,
    /// The then branch.
    pub then_block: Block,
    /// The else branch, when written.
    pub else_block: Option<Block>,
    /// Source range of the statement.
    pub span: Span,
}

/// One KSL expression node (full port of the Zig `Expr` union).
#[derive(Debug, Clone)]
pub enum Expr {
    /// An integer literal.
    Integer(IntegerLiteral),
    /// A floating-point literal.
    Float(FloatLiteral),
    /// A string literal.
    String(StringLiteral),
    /// A boolean literal.
    Bool(BoolLiteral),
    /// A (possibly qualified) identifier.
    Identifier(IdentifierExpr),
    /// A unary operation.
    Unary(UnaryExpr),
    /// A binary operation.
    Binary(BinaryExpr),
    /// A call expression.
    Call(CallExpr),
    /// A member access.
    Member(MemberExpr),
    /// An index access.
    Index(IndexExpr),
}

impl Expr {
    /// Returns the source range of any expression (the Zig `exprSpan`).
    pub fn span(&self) -> Span {
        match self {
            Expr::Integer(value) => value.span,
            Expr::Float(value) => value.span,
            Expr::String(value) => value.span,
            Expr::Bool(value) => value.span,
            Expr::Identifier(value) => value.span,
            Expr::Unary(value) => value.span,
            Expr::Binary(value) => value.span,
            Expr::Call(value) => value.span,
            Expr::Member(value) => value.span,
            Expr::Index(value) => value.span,
        }
    }
}

impl TypeRef {
    /// Returns the source range of any type reference (the Zig `typeSpan`).
    pub fn span(&self) -> Span {
        match self {
            TypeRef::Named(name) => name.span,
            TypeRef::RuntimeArray(array) => array.span,
        }
    }
}

/// An integer literal, kept as source text (KSL defers numeric parsing).
#[derive(Debug, Clone)]
pub struct IntegerLiteral {
    /// The literal's source text.
    pub text: String,
    /// Source range of the literal.
    pub span: Span,
}

/// A floating-point literal, kept as source text.
#[derive(Debug, Clone)]
pub struct FloatLiteral {
    /// The literal's source text.
    pub text: String,
    /// Source range of the literal.
    pub span: Span,
}

/// A string literal, kept as source text.
#[derive(Debug, Clone)]
pub struct StringLiteral {
    /// The literal's source text.
    pub text: String,
    /// Source range of the literal.
    pub span: Span,
}

/// A boolean literal.
#[derive(Debug, Clone, Copy)]
pub struct BoolLiteral {
    /// The literal value.
    pub value: bool,
    /// Source range of the literal.
    pub span: Span,
}

/// A (possibly qualified) identifier in expression position.
#[derive(Debug, Clone)]
pub struct IdentifierExpr {
    /// The dotted name path.
    pub name: QualifiedName,
    /// Source range of the expression.
    pub span: Span,
}

/// A unary operation.
#[derive(Debug, Clone)]
pub struct UnaryExpr {
    /// The operator.
    pub op: UnaryOp,
    /// The operand.
    pub operand: ExprId,
    /// Source range of the expression.
    pub span: Span,
}

/// KSL unary operators.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UnaryOp {
    /// Arithmetic negation `-`.
    Neg,
    /// Logical not `!`.
    Not,
}

/// A binary operation.
#[derive(Debug, Clone)]
pub struct BinaryExpr {
    /// The operator.
    pub op: BinaryOp,
    /// Left operand.
    pub left: ExprId,
    /// Right operand.
    pub right: ExprId,
    /// Source range of the expression.
    pub span: Span,
}

/// KSL binary operators (full port of the Zig `BinaryOp`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BinaryOp {
    /// `+`
    Add,
    /// `-`
    Sub,
    /// `*`
    Mul,
    /// `/`
    Div,
    /// `<`
    Less,
    /// `<=`
    LessEqual,
    /// `>`
    Greater,
    /// `>=`
    GreaterEqual,
    /// `==`
    Equal,
    /// `!=`
    NotEqual,
}

/// A call `callee(args)`.
#[derive(Debug, Clone)]
pub struct CallExpr {
    /// The called expression.
    pub callee: ExprId,
    /// The arguments in source order.
    pub args: Vec<ExprId>,
    /// Source range of the expression.
    pub span: Span,
}

/// A member access `object.name`.
#[derive(Debug, Clone)]
pub struct MemberExpr {
    /// The accessed object.
    pub object: ExprId,
    /// The member name.
    pub name: Symbol,
    /// Source range of the expression.
    pub span: Span,
}

/// An index access `object[index]`.
#[derive(Debug, Clone)]
pub struct IndexExpr {
    /// The indexed object.
    pub object: ExprId,
    /// The index expression.
    pub index: ExprId,
    /// Source range of the expression.
    pub span: Span,
}
