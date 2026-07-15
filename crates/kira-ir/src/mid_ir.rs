//! Place/value mid IR — the representation the ownership checker runs on.
//!
//! Ported from kira-zig `kira_ir/src/mid_ir.zig`. Structured statements over
//! [`Place`]s and [`Value`] trees, between HIR and the low IR.
//!
//! Scaffold status: containers, statements, and places are ported; the
//! [`Value`] enum carries the representative variants below plus a TODO for
//! the long tail. Zig `*Value` pointers become boxed values (rare, checker-
//! internal trees; the flat arenas live in HIR and low IR).
//!
//! TODO(port) remaining `Value` variants: `callback`, `call_value`,
//! `construct`, `construct_enum_variant`, `array`, `builder_array`, `binary`,
//! `unary`, `cast`, `conditional`, the `native_*` / `c_string_to_string` /
//! `array_len` / `string_*` unary-wrapper values, `opaque_member`,
//! `opaque_index`, and the task spine values (`task_spawn`,
//! `task_spawn_ready`, `task_await`, `task_cancel`, `task_detach`,
//! `task_yield`, `task_sleep`).

use kira_runtime_abi::FunctionExecution;
use kira_semantics_model::{OwnershipMode, ResolvedType, Span};

/// A mid-IR program wrapping its source HIR (Zig `Program`).
///
/// TODO(port): the Zig struct embeds `source_program: model.Program`; wire it
/// once program construction is ported.
#[derive(Debug, Clone, Default)]
pub struct Program {
    /// Zig `functions: []Function`.
    pub functions: Vec<Function>,
    /// Zig `entry_index: usize`.
    pub entry_index: usize,
}

/// A program that passed the mid-IR ownership checker (Zig `CheckedProgram`).
#[derive(Debug, Clone, Default)]
pub struct CheckedProgram {
    /// Zig `program: Program`.
    pub program: Program,
}

/// A mid-IR function (Zig `Function`).
#[derive(Debug, Clone)]
pub struct Function {
    /// Zig `id: u32`.
    pub id: u32,
    /// Zig `name: []const u8`.
    pub name: String,
    /// Zig `execution: FunctionExecution`.
    pub execution: FunctionExecution,
    /// Zig `is_extern: bool = false`.
    pub is_extern: bool,
    /// Zig `params: []const Parameter`.
    pub params: Vec<Parameter>,
    /// Zig `locals: []const Local`.
    pub locals: Vec<Local>,
    /// Zig `captures: []const Capture`.
    pub captures: Vec<Capture>,
    /// Zig `return_type: ResolvedType`.
    pub return_type: ResolvedType,
    /// Zig `return_ownership: OwnershipMode = .owned`.
    pub return_ownership: OwnershipMode,
    /// Zig `body: Block`.
    pub body: Block,
    /// Zig `span: Span`.
    pub span: Span,
}

/// A function parameter (Zig `Parameter`).
#[derive(Debug, Clone)]
pub struct Parameter {
    /// Zig `id: u32`.
    pub id: u32,
    /// Zig `name: []const u8`.
    pub name: String,
    /// Zig `ty: ResolvedType`.
    pub ty: ResolvedType,
    /// Zig `ownership: OwnershipMode = .owned`.
    pub ownership: OwnershipMode,
    /// Zig `span: Span`.
    pub span: Span,
}

/// A function-local slot (Zig `Local`).
#[derive(Debug, Clone)]
pub struct Local {
    /// Zig `id: u32`.
    pub id: u32,
    /// Zig `name: []const u8`.
    pub name: String,
    /// Zig `ty: ResolvedType`.
    pub ty: ResolvedType,
    /// Zig `ownership: OwnershipMode = .owned`.
    pub ownership: OwnershipMode,
    /// Zig `is_parameter: bool = false`.
    pub is_parameter: bool,
    /// Zig `is_capture: bool = false`.
    pub is_capture: bool,
    /// Zig `span: Span`.
    pub span: Span,
}

/// A closure capture (Zig `Capture`).
#[derive(Debug, Clone)]
pub struct Capture {
    /// Zig `local_id: u32`.
    pub local_id: u32,
    /// Zig `source_local_id: u32`.
    pub source_local_id: u32,
    /// Zig `by_ref: bool = false`.
    pub by_ref: bool,
    /// Zig `ownership: OwnershipMode = .borrow_read`.
    pub ownership: OwnershipMode,
    /// Zig `name: []const u8`.
    pub name: String,
    /// Zig `ty: ResolvedType`.
    pub ty: ResolvedType,
    /// Zig `span: Span`.
    pub span: Span,
}

/// A statement block (Zig `Block`).
#[derive(Debug, Clone)]
pub struct Block {
    /// Zig `statements: []Statement`.
    pub statements: Vec<Statement>,
    /// Zig `span: Span`.
    pub span: Span,
}

/// A mid-IR statement (Zig `Statement`).
#[derive(Debug, Clone)]
pub enum Statement {
    /// Zig `.let_stmt`.
    Let(LetStatement),
    /// Zig `.assign_stmt`.
    Assign(AssignStatement),
    /// Zig `.expr_stmt`.
    Expr(ExprStatement),
    /// Zig `.if_stmt`.
    If(IfStatement),
    /// Zig `.for_stmt`.
    For(ForStatement),
    /// Zig `.while_stmt`.
    While(WhileStatement),
    /// Zig `.break_stmt`.
    Break(Span),
    /// Zig `.continue_stmt`.
    Continue(Span),
    /// Zig `.match_stmt`.
    Match(MatchStatement),
    /// Zig `.switch_stmt`.
    Switch(SwitchStatement),
    /// Zig `.return_stmt`.
    Return(ReturnStatement),
}

/// `let` binding (Zig `LetStatement`).
#[derive(Debug, Clone)]
pub struct LetStatement {
    /// Zig `local: Local`.
    pub local: Local,
    /// Zig `value: ?Value`.
    pub value: Option<Value>,
    /// Zig `is_reborrow: bool = false`.
    pub is_reborrow: bool,
    /// Zig `span: Span`.
    pub span: Span,
}

/// Assignment into a place (Zig `AssignStatement`).
#[derive(Debug, Clone)]
pub struct AssignStatement {
    /// Zig `target: Place`.
    pub target: Place,
    /// Zig `value: Value`.
    pub value: Value,
    /// Zig `span: Span`.
    pub span: Span,
}

/// Expression statement (Zig `ExprStatement`).
#[derive(Debug, Clone)]
pub struct ExprStatement {
    /// Zig `value: Value`.
    pub value: Value,
    /// Zig `span: Span`.
    pub span: Span,
}

/// `if`/`else` (Zig `IfStatement`).
#[derive(Debug, Clone)]
pub struct IfStatement {
    /// Zig `condition: Value`.
    pub condition: Value,
    /// Zig `then_block: Block`.
    pub then_block: Block,
    /// Zig `else_block: ?Block`.
    pub else_block: Option<Block>,
    /// Zig `span: Span`.
    pub span: Span,
}

/// `for` loop (Zig `ForStatement`).
#[derive(Debug, Clone)]
pub struct ForStatement {
    /// Zig `binding: Local`.
    pub binding: Local,
    /// Zig `iterator: Value`.
    pub iterator: Value,
    /// Zig `body: Block`.
    pub body: Block,
    /// Zig `span: Span`.
    pub span: Span,
}

/// `while` loop (Zig `WhileStatement`).
#[derive(Debug, Clone)]
pub struct WhileStatement {
    /// Zig `condition: Value`.
    pub condition: Value,
    /// Zig `body: Block`.
    pub body: Block,
    /// Zig `span: Span`.
    pub span: Span,
}

/// Enum `match` (Zig `MatchStatement`).
#[derive(Debug, Clone)]
pub struct MatchStatement {
    /// Zig `subject: Value`.
    pub subject: Value,
    /// Zig `arms: []MatchArm`.
    pub arms: Vec<MatchArm>,
    /// Zig `span: Span`.
    pub span: Span,
}

/// One `match` arm (Zig `MatchArm`).
#[derive(Debug, Clone)]
pub struct MatchArm {
    /// Zig `bound_locals: []const Local`.
    pub bound_locals: Vec<Local>,
    /// Zig `guard: ?Value`.
    pub guard: Option<Value>,
    /// Zig `body: Block`.
    pub body: Block,
    /// Zig `span: Span`.
    pub span: Span,
}

/// Value `switch` (Zig `SwitchStatement`).
#[derive(Debug, Clone)]
pub struct SwitchStatement {
    /// Zig `subject: Value`.
    pub subject: Value,
    /// Zig `cases: []SwitchCase`.
    pub cases: Vec<SwitchCase>,
    /// Zig `default_block: ?Block`.
    pub default_block: Option<Block>,
    /// Zig `span: Span`.
    pub span: Span,
}

/// One `switch` case (Zig `SwitchCase`).
#[derive(Debug, Clone)]
pub struct SwitchCase {
    /// Zig `pattern: Value`.
    pub pattern: Value,
    /// Zig `body: Block`.
    pub body: Block,
    /// Zig `span: Span`.
    pub span: Span,
}

/// `return` (Zig `ReturnStatement`).
#[derive(Debug, Clone)]
pub struct ReturnStatement {
    /// Zig `return_place: Place`.
    pub return_place: Place,
    /// Zig `value: ?Value`.
    pub value: Option<Value>,
    /// Zig `span: Span`.
    pub span: Span,
}

/// A mid-IR value tree (Zig `Value`) — representative scaffold; see the
/// module docs for the TODO long tail.
#[derive(Debug, Clone)]
pub enum Value {
    /// Zig `.integer`.
    Integer(LiteralValue),
    /// Zig `.float`.
    Float(LiteralValue),
    /// Zig `.string`.
    String(LiteralValue),
    /// Zig `.boolean`.
    Boolean(LiteralValue),
    /// Zig `.null_ptr`.
    NullPtr(LiteralValue),
    /// Zig `.function_ref`.
    FunctionRef(FunctionRefValue),
    /// Zig `.place` — a read of a place with an ownership mode.
    Place(PlaceValue),
    /// Zig `.namespace_ref`.
    NamespaceRef(NamespaceRefValue),
    /// Zig `.call`.
    Call(CallValue),
    /// Zig `.virtual_call`.
    VirtualCall(VirtualCallValue),
}

/// A literal leaf value (Zig `IntegerValue`/`FloatValue`/`StringValue`/
/// `BooleanValue`/`NullPtrValue` — all `{ ty, span }` shells; the literal
/// payloads live in the HIR the mid IR wraps).
#[derive(Debug, Clone)]
pub struct LiteralValue {
    /// Zig `ty: ResolvedType`.
    pub ty: ResolvedType,
    /// Zig `span: Span`.
    pub span: Span,
}

/// Reference to a named function (Zig `FunctionRefValue`).
#[derive(Debug, Clone)]
pub struct FunctionRefValue {
    /// Zig `function_id: u32`.
    pub function_id: u32,
    /// Zig `name: []const u8`.
    pub name: String,
    /// Zig `ty: ResolvedType`.
    pub ty: ResolvedType,
    /// Zig `span: Span`.
    pub span: Span,
}

/// Namespace path reference (Zig `NamespaceRefValue`).
#[derive(Debug, Clone)]
pub struct NamespaceRefValue {
    /// Zig `path: []const u8`.
    pub path: String,
    /// Zig `ty: ResolvedType`.
    pub ty: ResolvedType,
    /// Zig `span: Span`.
    pub span: Span,
}

/// A place read (Zig `PlaceValue`).
#[derive(Debug, Clone)]
pub struct PlaceValue {
    /// Zig `place: Place`.
    pub place: Place,
    /// Zig `ownership: OwnershipMode = .borrow_read`.
    pub ownership: OwnershipMode,
}

/// A direct call (Zig `CallValue`).
#[derive(Debug, Clone)]
pub struct CallValue {
    /// Zig `callee_name: []const u8`.
    pub callee_name: String,
    /// Zig `function_id: ?u32`.
    pub function_id: Option<u32>,
    /// Zig `args: []Value`.
    pub args: Vec<Value>,
    /// Zig `param_ownership: []const OwnershipMode`.
    pub param_ownership: Vec<OwnershipMode>,
    /// Zig `return_ownership: OwnershipMode = .owned`.
    pub return_ownership: OwnershipMode,
    /// Zig `ty: ResolvedType`.
    pub ty: ResolvedType,
    /// Zig `temp_id: u32`.
    pub temp_id: u32,
    /// Zig `span: Span`.
    pub span: Span,
}

/// A virtual (family-dispatched) call (Zig `VirtualCallValue`).
#[derive(Debug, Clone)]
pub struct VirtualCallValue {
    /// Zig `receiver: *Value`.
    pub receiver: Box<Value>,
    /// Zig `receiver_ownership: OwnershipMode = .borrow_read`.
    pub receiver_ownership: OwnershipMode,
    /// Zig `static_type_name: []const u8`.
    pub static_type_name: String,
    /// Zig `method_name: []const u8`.
    pub method_name: String,
    /// Zig `args: []Value`.
    pub args: Vec<Value>,
    /// Zig `param_ownership: []const OwnershipMode`.
    pub param_ownership: Vec<OwnershipMode>,
    /// Zig `return_ownership: OwnershipMode = .owned`.
    pub return_ownership: OwnershipMode,
    /// Zig `ty: ResolvedType`.
    pub ty: ResolvedType,
    /// Zig `temp_id: u32`.
    pub temp_id: u32,
    /// Zig `span: Span`.
    pub span: Span,
}

/// A place: a root plus a projection path (Zig `Place`).
#[derive(Debug, Clone)]
pub struct Place {
    /// Zig `root: Root`.
    pub root: PlaceRoot,
    /// Zig `projections: []Projection`.
    pub projections: Vec<Projection>,
    /// Zig `ty: ResolvedType`.
    pub ty: ResolvedType,
    /// Zig `span: Span`.
    pub span: Span,
}

/// Root of a place (Zig `Place.Root`).
#[derive(Debug, Clone)]
pub enum PlaceRoot {
    /// Zig `.local: u32`.
    Local(u32),
    /// Zig `.capture: u32`.
    Capture(u32),
    /// Zig `.return_slot`.
    ReturnSlot,
}

/// One projection step (Zig `Projection`).
#[derive(Debug, Clone)]
pub enum Projection {
    /// Zig `.field`.
    Field(FieldProjection),
    /// Zig `.index`.
    Index(IndexProjection),
    /// Zig `.parent_view`.
    ParentView(ParentViewProjection),
}

/// Field projection (Zig `FieldProjection`).
#[derive(Debug, Clone)]
pub struct FieldProjection {
    /// Zig `container_type_name: []const u8`.
    pub container_type_name: String,
    /// Zig `field_name: []const u8`.
    pub field_name: String,
    /// Zig `field_index: u32`.
    pub field_index: u32,
    /// Zig `ty: ResolvedType`.
    pub ty: ResolvedType,
    /// Zig `span: Span`.
    pub span: Span,
}

/// Index projection (Zig `IndexProjection`).
#[derive(Debug, Clone)]
pub struct IndexProjection {
    /// Zig `index: ?i64` — static index when known.
    pub index: Option<i64>,
    /// Zig `dynamic_index: ?*Value`.
    pub dynamic_index: Option<Box<Value>>,
    /// Zig `ty: ResolvedType`.
    pub ty: ResolvedType,
    /// Zig `span: Span`.
    pub span: Span,
}

/// Parent-view (upcast) projection (Zig `ParentViewProjection`).
#[derive(Debug, Clone)]
pub struct ParentViewProjection {
    /// Zig `offset: u32`.
    pub offset: u32,
    /// Zig `ty: ResolvedType`.
    pub ty: ResolvedType,
    /// Zig `span: Span`.
    pub span: Span,
}
