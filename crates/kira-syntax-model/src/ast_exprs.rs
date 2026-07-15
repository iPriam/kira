//! Expression, statement, type-expression, and builder-block AST nodes.
//!
//! Mirrors kira-zig `packages/kira_syntax_model/src/ast_exprs.zig`, translated
//! to the index/arena pattern: Zig `*Expr` / `*TypeExpr` / `*MatchPattern`
//! pointers become [`ExprId`] / [`TypeExprId`] / [`PatternId`] indices into
//! [`crate::ast::AstArenas`]; name slices become interned [`Symbol`]s; decoded
//! literal and raw captured text stays owned `String`.

use kira_core::Symbol;
use kira_source::Span;
use la_arena::Idx;

use crate::ast::{Annotation, FieldStorage, QualifiedName};

/// Arena index of an [`Expr`].
pub type ExprId = Idx<Expr>;
/// Arena index of a [`TypeExpr`].
pub type TypeExprId = Idx<TypeExpr>;
/// Arena index of a [`MatchPattern`].
pub type PatternId = Idx<MatchPattern>;

/// One statement inside a block.
#[derive(Debug, Clone)]
pub enum Statement {
    /// `let` / `var` binding.
    Let(LetStatement),
    /// Assignment to an lvalue.
    Assign(AssignStatement),
    /// A bare expression evaluated for effect.
    Expr(ExprStatement),
    /// `return` with optional value.
    Return(ReturnStatement),
    /// `if` / `else`.
    If(IfStatement),
    /// `for x in ...`.
    For(ForStatement),
    /// `while`.
    While(WhileStatement),
    /// `break`.
    Break(BreakStatement),
    /// `continue`.
    Continue(ContinueStatement),
    /// `match` over enum variants.
    Match(MatchStatement),
    /// `switch` over values.
    Switch(SwitchStatement),
    /// `attempt { ... } handle ...` linear error handling.
    Attempt(AttemptStatement),
}

/// Linear error handling over `Result<Value, Failure>`: each `try` inside
/// `body` unwraps a `Result`; on `Error` control transfers to a `handle` case.
#[derive(Debug, Clone)]
pub struct AttemptStatement {
    /// Statements executed inside the attempt block.
    pub body: Vec<Statement>,
    /// The `handle` cases, one per failure variant.
    pub handlers: Vec<HandleCase>,
    /// Source range of the whole statement.
    pub span: Span,
}

/// One `handle Variant(binding) { ... }` case of an attempt statement.
#[derive(Debug, Clone)]
pub struct HandleCase {
    /// The failure variant this case handles.
    pub variant_name: Symbol,
    /// Optional binding for the variant payload.
    pub binding_name: Option<Symbol>,
    /// The handler body.
    pub body: Block,
    /// Source range of the case.
    pub span: Span,
}

/// A `let name: Type = value` (or `var`) binding statement.
#[derive(Debug, Clone)]
pub struct LetStatement {
    /// Annotations preceding the binding.
    pub annotations: Vec<Annotation>,
    /// `let` (immutable) vs `var` (mutable).
    pub storage: FieldStorage,
    /// Name being bound.
    pub name: Symbol,
    /// Declared type, when written.
    pub type_expr: Option<TypeExprId>,
    /// Initializer, when written.
    pub value: Option<ExprId>,
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

/// A `for binding in iterator { ... }` loop (or `for i in start..end`).
#[derive(Debug, Clone)]
pub struct ForStatement {
    /// The loop binding name.
    pub binding_name: Symbol,
    /// The iterable, or the START bound of a numeric range when `range_end` is set.
    pub iterator: ExprId,
    /// The end bound of a numeric range `start..end`, when written.
    pub range_end: Option<ExprId>,
    /// The loop body.
    pub body: Block,
    /// Source range of the statement.
    pub span: Span,
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

/// A `break` statement.
#[derive(Debug, Clone, Copy)]
pub struct BreakStatement {
    /// Source range of the statement.
    pub span: Span,
}

/// A `continue` statement.
#[derive(Debug, Clone, Copy)]
pub struct ContinueStatement {
    /// Source range of the statement.
    pub span: Span,
}

/// A `match subject { ... }` statement over enum variants.
#[derive(Debug, Clone)]
pub struct MatchStatement {
    /// The matched value.
    pub subject: ExprId,
    /// The match arms in source order.
    pub arms: Vec<MatchArm>,
    /// Source range of the statement.
    pub span: Span,
}

/// One arm of a `match`: patterns, optional guard, and body.
#[derive(Debug, Clone)]
pub struct MatchArm {
    /// The `|`-separated patterns this arm covers.
    pub patterns: Vec<MatchPattern>,
    /// Optional `if` guard.
    pub guard: Option<ExprId>,
    /// The arm body.
    pub body: Block,
    /// Source range of the arm.
    pub span: Span,
}

/// A pattern in a `match` arm.
#[derive(Debug, Clone)]
pub enum MatchPattern {
    /// A bare variant name, e.g. `Ok`.
    BareVariant {
        /// The variant name.
        name: Symbol,
        /// Source range of the pattern.
        span: Span,
    },
    /// A destructuring pattern, e.g. `Ok(inner)`.
    Destructure {
        /// The variant being destructured.
        variant_name: Symbol,
        /// The nested pattern for the payload.
        inner: PatternId,
        /// Source range of the pattern.
        span: Span,
    },
    /// An `inner as binding` pattern.
    AsBinding {
        /// The nested pattern.
        inner: PatternId,
        /// The introduced binding name.
        binding_name: Symbol,
        /// Source range of the pattern.
        span: Span,
    },
}

/// A `switch subject { case ... default ... }` statement.
#[derive(Debug, Clone)]
pub struct SwitchStatement {
    /// The switched value.
    pub subject: ExprId,
    /// The `case` clauses in source order.
    pub cases: Vec<SwitchCase>,
    /// The `default` clause, when written.
    pub default_block: Option<Block>,
    /// Source range of the statement.
    pub span: Span,
}

/// One `case pattern { ... }` clause of a switch.
#[derive(Debug, Clone)]
pub struct SwitchCase {
    /// The compared value.
    pub pattern: ExprId,
    /// The clause body.
    pub body: Block,
    /// Source range of the clause.
    pub span: Span,
}

/// A declarative UI builder block: `{ child; child; ... }`.
#[derive(Debug, Clone, Default)]
pub struct BuilderBlock {
    /// The items in source order.
    pub items: Vec<BuilderItem>,
    /// Source range of the block.
    pub span: Span,
}

/// One item of a builder block.
#[derive(Debug, Clone)]
pub enum BuilderItem {
    /// A child expression.
    Expr(BuilderExprItem),
    /// A conditional group of children.
    If(BuilderIfItem),
    /// A repeated group of children.
    For(BuilderForItem),
    /// A switched group of children.
    Switch(BuilderSwitchItem),
    /// A `let name = value` member inside a construction's trailing block;
    /// semantics turns it into a labeled field argument.
    FieldOverride(BuilderFieldOverrideItem),
}

/// A child expression inside a builder block.
#[derive(Debug, Clone)]
pub struct BuilderExprItem {
    /// The child expression.
    pub expr: ExprId,
    /// Source range of the item.
    pub span: Span,
}

/// A `let name = value` field override inside a construction's trailing block.
#[derive(Debug, Clone)]
pub struct BuilderFieldOverrideItem {
    /// The overridden field name.
    pub name: Symbol,
    /// The override value.
    pub value: ExprId,
    /// Source range of the item.
    pub span: Span,
}

/// An `if` inside a builder block.
#[derive(Debug, Clone)]
pub struct BuilderIfItem {
    /// The condition expression.
    pub condition: ExprId,
    /// Children produced when the condition holds.
    pub then_block: BuilderBlock,
    /// Children produced otherwise, when written.
    pub else_block: Option<BuilderBlock>,
    /// Source range of the item.
    pub span: Span,
}

/// A `for` inside a builder block.
#[derive(Debug, Clone)]
pub struct BuilderForItem {
    /// The loop binding name.
    pub binding_name: Symbol,
    /// The iterable expression.
    pub iterator: ExprId,
    /// Children produced per element.
    pub body: BuilderBlock,
    /// Source range of the item.
    pub span: Span,
}

/// A `switch` inside a builder block.
#[derive(Debug, Clone)]
pub struct BuilderSwitchItem {
    /// The switched value.
    pub subject: ExprId,
    /// The `case` clauses in source order.
    pub cases: Vec<BuilderSwitchCase>,
    /// The `default` clause, when written.
    pub default_block: Option<BuilderBlock>,
    /// Source range of the item.
    pub span: Span,
}

/// One `case` clause of a builder switch.
#[derive(Debug, Clone)]
pub struct BuilderSwitchCase {
    /// The compared value.
    pub pattern: ExprId,
    /// Children produced for this case.
    pub body: BuilderBlock,
    /// Source range of the clause.
    pub span: Span,
}

/// One expression node (full port of the Zig `Expr` union).
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
    /// A `.name` implicit member, resolved from the expected type.
    ImplicitMember(ImplicitMemberExpr),
    /// An array literal `[a, b, c]`.
    Array(ArrayExpr),
    /// An array built from a builder block.
    BuilderArray(BuilderArrayExpr),
    /// A `{ params in body }` callback block.
    Callback(CallbackBlock),
    /// A struct literal `Type { field: value }`.
    StructLiteral(StructLiteralExpr),
    /// `nativeState(value)` — boxes a value for native FFI state.
    NativeState(NativeStateExpr),
    /// `nativeUserData(state)` — extracts the raw userdata token.
    NativeUserData(NativeUserDataExpr),
    /// `nativeRecover(Type, value)` — recovers a typed view of native state.
    NativeRecover(NativeRecoverExpr),
    /// `nativeStateFree(state)` — releases a native-state box.
    NativeStateFree(NativeStateFreeExpr),
    /// An explicit `move` / `copy` ownership operation.
    Ownership(OwnershipExpr),
    /// A unary operation.
    Unary(UnaryExpr),
    /// A binary operation.
    Binary(BinaryExpr),
    /// A `condition ? then : else` conditional.
    Conditional(ConditionalExpr),
    /// A member access `object.member`.
    Member(MemberExpr),
    /// An index access `object[index]`.
    Index(IndexExpr),
    /// A call (or construction, or `name!(...)` macro call).
    Call(CallExpr),
    /// A `try expr` Result unwrap (only inside `attempt`).
    Try(TryExpr),
    /// A `quote { ... }` block inside a procedural macro body.
    Quote(QuoteExpr),
}

/// A `quote { ... }` template: literal token runs interleaved with `#{ expr }`
/// splices, rendered to Kira source at macro-expansion time.
#[derive(Debug, Clone)]
pub struct QuoteExpr {
    /// Text runs and splice holes in source order.
    pub parts: Vec<QuotePart>,
    /// Source range of the quote block.
    pub span: Span,
}

/// One part of a quote template.
#[derive(Debug, Clone)]
pub enum QuotePart {
    /// Literal source text (captured token lexemes joined by spaces).
    Text(String),
    /// A `#{ expr }` splice evaluated at macro-expansion time.
    Splice(ExprId),
}

/// `try expr` — unwraps a `Result`, transferring to a `handle` case on error.
#[derive(Debug, Clone)]
pub struct TryExpr {
    /// The `Result`-typed operand.
    pub operand: ExprId,
    /// Source range of the expression.
    pub span: Span,
}

/// An integer literal with its parsed value.
#[derive(Debug, Clone, Copy)]
pub struct IntegerLiteral {
    /// The parsed value.
    pub value: i64,
    /// Source range of the literal.
    pub span: Span,
}

/// A floating-point literal with its parsed value.
#[derive(Debug, Clone, Copy)]
pub struct FloatLiteral {
    /// The parsed value.
    pub value: f64,
    /// Source range of the literal.
    pub span: Span,
}

/// A string literal with its decoded (unescaped) value.
#[derive(Debug, Clone)]
pub struct StringLiteral {
    /// The decoded string contents.
    pub value: String,
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

/// `.name` in expression position; semantics resolves it from the expected
/// result type (the parser records no synthetic namespace).
#[derive(Debug, Clone, Copy)]
pub struct ImplicitMemberExpr {
    /// The member name after the dot.
    pub name: Symbol,
    /// Source range of the expression.
    pub span: Span,
}

/// An array literal `[a, b, c]`.
#[derive(Debug, Clone)]
pub struct ArrayExpr {
    /// The element expressions in source order.
    pub elements: Vec<ExprId>,
    /// Source range of the literal.
    pub span: Span,
}

/// An array literal produced by a builder block.
#[derive(Debug, Clone)]
pub struct BuilderArrayExpr {
    /// The builder producing the elements.
    pub builder: BuilderBlock,
    /// Source range of the expression.
    pub span: Span,
}

/// A struct literal `Type { field: value, ... }`.
#[derive(Debug, Clone)]
pub struct StructLiteralExpr {
    /// The struct type being constructed.
    pub type_name: QualifiedName,
    /// The field initializers in source order.
    pub fields: Vec<StructLiteralField>,
    /// Source range of the literal.
    pub span: Span,
}

/// One `field: value` initializer of a struct literal.
#[derive(Debug, Clone)]
pub struct StructLiteralField {
    /// The initialized field.
    pub name: Symbol,
    /// The field value.
    pub value: ExprId,
    /// Source range of the initializer.
    pub span: Span,
}

/// `nativeState(value)` — boxes a value for native FFI state.
#[derive(Debug, Clone)]
pub struct NativeStateExpr {
    /// The boxed value.
    pub value: ExprId,
    /// Source range of the expression.
    pub span: Span,
}

/// `nativeUserData(state)` — extracts the `RawPtr` userdata token of a state box.
#[derive(Debug, Clone)]
pub struct NativeUserDataExpr {
    /// The `NativeState<T>` handle.
    pub state: ExprId,
    /// Source range of the expression.
    pub span: Span,
}

/// `nativeRecover(Type, value)` — recovers a typed view from a userdata token.
#[derive(Debug, Clone)]
pub struct NativeRecoverExpr {
    /// The state type to recover as.
    pub state_type: TypeExprId,
    /// The `RawPtr` userdata token.
    pub value: ExprId,
    /// Source range of the expression.
    pub span: Span,
}

/// `nativeStateFree(state)` — releases a native-state box; outstanding
/// `nativeRecover` views become invalid.
#[derive(Debug, Clone)]
pub struct NativeStateFreeExpr {
    /// The `NativeState<T>` handle or `RawPtr` userdata token.
    pub state: ExprId,
    /// Source range of the expression.
    pub span: Span,
}

/// An explicit `move value` / `copy value` ownership operation.
#[derive(Debug, Clone)]
pub struct OwnershipExpr {
    /// Move or copy.
    pub op: OwnershipExprOp,
    /// The operand.
    pub operand: ExprId,
    /// Source range of the expression.
    pub span: Span,
}

/// The two explicit ownership operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OwnershipExprOp {
    /// Transfer ownership.
    Move,
    /// Duplicate the value.
    Copy,
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

/// A binary operation.
#[derive(Debug, Clone)]
pub struct BinaryExpr {
    /// The operator.
    pub op: BinaryOp,
    /// Left operand.
    pub lhs: ExprId,
    /// Right operand.
    pub rhs: ExprId,
    /// Source range of the expression.
    pub span: Span,
}

/// A `condition ? then : else` conditional expression.
#[derive(Debug, Clone)]
pub struct ConditionalExpr {
    /// The condition.
    pub condition: ExprId,
    /// Value when the condition holds.
    pub then_expr: ExprId,
    /// Value otherwise.
    pub else_expr: ExprId,
    /// Source range of the expression.
    pub span: Span,
}

/// A member access `object.member`.
#[derive(Debug, Clone)]
pub struct MemberExpr {
    /// The accessed object.
    pub object: ExprId,
    /// The member name.
    pub member: Symbol,
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

/// A call `callee(args)`, optionally with a trailing builder or callback
/// block; `is_macro` marks a `name!(args)` macro call consumed before semantics.
#[derive(Debug, Clone)]
pub struct CallExpr {
    /// The called expression (a macro name when `is_macro`).
    pub callee: ExprId,
    /// The arguments in source order.
    pub args: Vec<CallArg>,
    /// A trailing `{ children }` builder block, when written.
    pub trailing_builder: Option<BuilderBlock>,
    /// A trailing `{ params in body }` callback block, when written.
    pub trailing_callback: Option<CallbackBlock>,
    /// True for a `name!(args)` macro call.
    pub is_macro: bool,
    /// Source range of the expression.
    pub span: Span,
}

/// One (optionally labeled) call argument.
#[derive(Debug, Clone)]
pub struct CallArg {
    /// The argument label, when written.
    pub label: Option<Symbol>,
    /// The argument value.
    pub value: ExprId,
    /// Source range of the argument.
    pub span: Span,
}

/// Binary operators (full port of the Zig `BinaryOp`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BinaryOp {
    /// `+`
    Add,
    /// `-`
    Subtract,
    /// `*`
    Multiply,
    /// `/`
    Divide,
    /// `%`
    Modulo,
    /// `==`
    Equal,
    /// `!=`
    NotEqual,
    /// `<`
    Less,
    /// `<=`
    LessEqual,
    /// `>`
    Greater,
    /// `>=`
    GreaterEqual,
    /// `&&`
    LogicalAnd,
    /// `||`
    LogicalOr,
    /// `&`
    BitAnd,
    /// `|`
    BitOr,
    /// `^`
    BitXor,
    /// `<<`
    ShiftLeft,
    /// `>>`
    ShiftRight,
}

/// Unary operators (full port of the Zig `UnaryOp`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UnaryOp {
    /// Arithmetic negation `-`.
    Negate,
    /// Logical not `!`.
    Not,
    /// Bitwise not `~`.
    BitNot,
}

/// One type expression node (full port of the Zig `TypeExpr` union).
#[derive(Debug, Clone)]
pub enum TypeExpr {
    /// A named type, possibly qualified.
    Named(QualifiedName),
    /// A generic instantiation `Base<Args>`.
    Generic(GenericTypeExpr),
    /// An ownership-annotated type.
    Ownership(OwnershipTypeExpr),
    /// An `any` / `some` existential type.
    Any(AnyTypeExpr),
    /// An array type `[T]`.
    Array(ArrayTypeExpr),
    /// A function type `(Params) -> Result`.
    Function(FunctionTypeExpr),
}

/// Ownership modes a type position can carry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OwnershipMode {
    /// Owned value.
    Owned,
    /// Shared (read) borrow.
    BorrowRead,
    /// Exclusive (mutable) borrow.
    BorrowMut,
    /// Moved-in value.
    Move,
    /// Copied-in value.
    Copy,
}

/// An ownership-annotated type, e.g. a borrow of the target type.
#[derive(Debug, Clone)]
pub struct OwnershipTypeExpr {
    /// The ownership mode.
    pub mode: OwnershipMode,
    /// The annotated target type.
    pub target: TypeExprId,
    /// Source range of the type expression.
    pub span: Span,
}

/// An `any Target` / `some Target` existential type; `existential`
/// distinguishes the `some` surface keyword (dynamic dispatch) from `any`.
#[derive(Debug, Clone)]
pub struct AnyTypeExpr {
    /// The constrained target type.
    pub target: TypeExprId,
    /// Source range of the type expression.
    pub span: Span,
    /// True for `some Target`.
    pub existential: bool,
}

/// An array type `[T]`.
#[derive(Debug, Clone)]
pub struct ArrayTypeExpr {
    /// The element type.
    pub element_type: TypeExprId,
    /// Source range of the type expression.
    pub span: Span,
}

/// A function type `(Params) -> Result`.
#[derive(Debug, Clone)]
pub struct FunctionTypeExpr {
    /// The parameter types in source order.
    pub params: Vec<TypeExprId>,
    /// The result type.
    pub result: TypeExprId,
    /// Source range of the type expression.
    pub span: Span,
}

/// A generic instantiation `Base<Args>`.
#[derive(Debug, Clone)]
pub struct GenericTypeExpr {
    /// The generic base name.
    pub base: QualifiedName,
    /// The type arguments in source order.
    pub args: Vec<TypeExprId>,
    /// Source range of the type expression.
    pub span: Span,
}

/// A `{ params in body }` callback block.
#[derive(Debug, Clone)]
pub struct CallbackBlock {
    /// The callback parameters in source order.
    pub params: Vec<CallbackParam>,
    /// The callback body.
    pub body: Block,
    /// Source range of the block.
    pub span: Span,
}

/// One parameter of a callback block.
#[derive(Debug, Clone, Copy)]
pub struct CallbackParam {
    /// The parameter name.
    pub name: Symbol,
    /// Source range of the parameter.
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
