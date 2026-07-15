//! HIR expressions — the typed expression tree, arena-allocated.
//!
//! Ported from kira-zig `kira_semantics_model/src/hir.zig` (expression half).
//! Zig `*Expr` pointers become [`ExprId`] indices into the program's
//! [`ExprArena`].

use crate::hir::{Parameter, Statement};
use crate::span::Span;
use crate::symbols::LocalSymbol;
use crate::types::{OwnershipMode, ResolvedType};

/// Arena index of an [`Expr`] (replaces Zig `*Expr`).
pub type ExprId = la_arena::Idx<Expr>;

/// Arena owning every [`Expr`] of a program.
pub type ExprArena = la_arena::Arena<Expr>;

/// A typed HIR expression (Zig `Expr`, `union(enum)`).
#[derive(Debug, Clone)]
pub enum Expr {
    /// Zig `.integer`.
    Integer(IntegerExpr),
    /// Zig `.float`.
    Float(FloatExpr),
    /// Zig `.string`.
    String(StringExpr),
    /// Zig `.boolean`.
    Boolean(BooleanExpr),
    /// Zig `.null_ptr`.
    NullPtr(NullPtrExpr),
    /// Zig `.function_ref`.
    FunctionRef(FunctionRefExpr),
    /// Zig `.callback`.
    Callback(CallbackExpr),
    /// Zig `.local`.
    Local(LocalExpr),
    /// Zig `.namespace_ref`.
    NamespaceRef(NamespaceRefExpr),
    /// Zig `.parent_view`.
    ParentView(ParentViewExpr),
    /// Zig `.c_string_to_string`.
    CStringToString(CStringToStringExpr),
    /// Zig `.array_len`.
    ArrayLen(ArrayLenExpr),
    /// Zig `.string_len`.
    StringLen(StringLenExpr),
    /// Zig `.string_from_scalar`.
    StringFromScalar(StringFromScalarExpr),
    /// Zig `.string_char_at`.
    StringCharAt(StringCharAtExpr),
    /// Zig `.string_substring`.
    StringSubstring(StringSubstringExpr),
    /// Zig `.string_index_of`.
    StringIndexOf(StringIndexOfExpr),
    /// Zig `.field`.
    Field(FieldExpr),
    /// Zig `.native_state`.
    NativeState(NativeStateExpr),
    /// Zig `.native_user_data`.
    NativeUserData(NativeUserDataExpr),
    /// Zig `.native_recover`.
    NativeRecover(NativeRecoverExpr),
    /// Zig `.native_state_free`.
    NativeStateFree(NativeStateFreeExpr),
    /// Zig `.binary`.
    Binary(BinaryExpr),
    /// Zig `.unary`.
    Unary(UnaryExpr),
    /// Zig `.cast`.
    Cast(CastExpr),
    /// Zig `.conditional`.
    Conditional(ConditionalExpr),
    /// Zig `.construct`.
    Construct(ConstructExpr),
    /// Zig `.construct_enum_variant`.
    ConstructEnumVariant(ConstructEnumVariantExpr),
    /// Zig `.call`.
    Call(CallExpr),
    /// Zig `.virtual_call`.
    VirtualCall(VirtualCallExpr),
    /// Zig `.call_value`.
    CallValue(CallValueExpr),
    /// Zig `.array`.
    Array(ArrayExpr),
    /// Zig `.builder_array`.
    BuilderArray(BuilderArrayExpr),
    /// Zig `.index`.
    Index(IndexExpr),
    /// Zig `.task_spawn` — `Task { f(a, b) }`: the call runs at first drive.
    TaskSpawn(TaskSpawnExpr),
    /// Zig `.task_spawn_ready` — `Task { <pure value> }`.
    TaskSpawnReady(TaskSpawnReadyExpr),
    /// Zig `.task_await`.
    TaskAwait(TaskAwaitExpr),
    /// Zig `.task_cancel`.
    TaskCancel(TaskCancelExpr),
    /// Zig `.task_detach`.
    TaskDetach(TaskDetachExpr),
    /// Zig `.task_yield`.
    TaskYield(TaskYieldExpr),
    /// Zig `.task_sleep`.
    TaskSleep(TaskSleepExpr),
}

/// Integer literal (Zig `IntegerExpr`).
#[derive(Debug, Clone)]
pub struct IntegerExpr {
    /// Zig `value: i64`.
    pub value: i64,
    /// Zig `ty: ResolvedType = .integer`.
    pub ty: ResolvedType,
    /// Zig `span: Span`.
    pub span: Span,
}

/// Float literal (Zig `FloatExpr`).
#[derive(Debug, Clone)]
pub struct FloatExpr {
    /// Zig `value: f64`.
    pub value: f64,
    /// Zig `ty: ResolvedType = .float`.
    pub ty: ResolvedType,
    /// Zig `span: Span`.
    pub span: Span,
}

/// String literal (Zig `StringExpr`).
#[derive(Debug, Clone)]
pub struct StringExpr {
    /// Zig `value: []const u8`.
    pub value: String,
    /// Zig `ty: ResolvedType = .string`.
    pub ty: ResolvedType,
    /// Zig `span: Span`.
    pub span: Span,
}

/// Boolean literal (Zig `BooleanExpr`).
#[derive(Debug, Clone)]
pub struct BooleanExpr {
    /// Zig `value: bool`.
    pub value: bool,
    /// Zig `ty: ResolvedType = .boolean`.
    pub ty: ResolvedType,
    /// Zig `span: Span`.
    pub span: Span,
}

/// Null raw pointer (Zig `NullPtrExpr`).
#[derive(Debug, Clone)]
pub struct NullPtrExpr {
    /// Zig `ty: ResolvedType`.
    pub ty: ResolvedType,
    /// Zig `span: Span`.
    pub span: Span,
}

/// How a function reference is materialized (Zig `FunctionRefRepresentation`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FunctionRefRepresentation {
    /// Zig `.callable_value`.
    #[default]
    CallableValue,
    /// Zig `.native_callback`.
    NativeCallback,
}

/// Reference to a named function (Zig `FunctionRefExpr`).
#[derive(Debug, Clone)]
pub struct FunctionRefExpr {
    /// Zig `representation: FunctionRefRepresentation = .callable_value`.
    pub representation: FunctionRefRepresentation,
    /// Zig `function_id: u32`.
    pub function_id: u32,
    /// Zig `name: []const u8`.
    pub name: String,
    /// Zig `ty: ResolvedType`.
    pub ty: ResolvedType,
    /// Zig `span: Span`.
    pub span: Span,
}

/// Read of a local (Zig `LocalExpr`).
#[derive(Debug, Clone)]
pub struct LocalExpr {
    /// Zig `local_id: u32`.
    pub local_id: u32,
    /// Zig `name: []const u8`.
    pub name: String,
    /// Zig `ty: ResolvedType`.
    pub ty: ResolvedType,
    /// Zig `storage: FieldStorage`.
    pub storage: crate::hir::FieldStorage,
    /// Zig `ownership: OwnershipMode = .borrow_read`.
    pub ownership: OwnershipMode,
    /// Zig `span: Span`.
    pub span: Span,
}

/// Namespace path reference (Zig `NamespaceRefExpr`).
#[derive(Debug, Clone)]
pub struct NamespaceRefExpr {
    /// Zig `root: []const u8`.
    pub root: String,
    /// Zig `path: []const u8`.
    pub path: String,
    /// Zig `ty: ResolvedType = .unknown`.
    pub ty: ResolvedType,
    /// Zig `span: Span`.
    pub span: Span,
}

/// Upcast view of a construct parent (Zig `ParentViewExpr`).
#[derive(Debug, Clone)]
pub struct ParentViewExpr {
    /// Zig `object: *Expr`.
    pub object: ExprId,
    /// Zig `ty: ResolvedType`.
    pub ty: ResolvedType,
    /// Zig `offset: u32`.
    pub offset: u32,
    /// Zig `span: Span`.
    pub span: Span,
}

/// `CString` -> `String` conversion (Zig `CStringToStringExpr`).
#[derive(Debug, Clone)]
pub struct CStringToStringExpr {
    /// Zig `value: *Expr`.
    pub value: ExprId,
    /// Zig `ty: ResolvedType = .string`.
    pub ty: ResolvedType,
    /// Zig `span: Span`.
    pub span: Span,
}

/// `array.len` (Zig `ArrayLenExpr`).
#[derive(Debug, Clone)]
pub struct ArrayLenExpr {
    /// Zig `object: *Expr`.
    pub object: ExprId,
    /// Zig `ty: ResolvedType = .integer`.
    pub ty: ResolvedType,
    /// Zig `span: Span`.
    pub span: Span,
}

/// `string.len` (Zig `StringLenExpr`).
#[derive(Debug, Clone)]
pub struct StringLenExpr {
    /// Zig `object: *Expr`.
    pub object: ExprId,
    /// Zig `ty: ResolvedType = .integer`.
    pub ty: ResolvedType,
    /// Zig `span: Span`.
    pub span: Span,
}

/// Byte format of a `String(x)` conversion (Zig `StringFromScalarSource`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StringFromScalarSource {
    /// Zig `.integer` — base-10.
    Integer,
    /// Zig `.float` — matches per-backend `print(float)`.
    Float,
    /// Zig `.boolean` — "true"/"false".
    Boolean,
}

/// `String(x)` scalar conversion (Zig `StringFromScalarExpr`).
#[derive(Debug, Clone)]
pub struct StringFromScalarExpr {
    /// Zig `operand: *Expr`.
    pub operand: ExprId,
    /// Zig `source_kind: StringFromScalarSource`.
    pub source_kind: StringFromScalarSource,
    /// Zig `ty: ResolvedType = .string`.
    pub ty: ResolvedType,
    /// Zig `span: Span`.
    pub span: Span,
}

/// `s.charAt(i)` — UTF-8 code unit at byte offset, as Int (Zig `StringCharAtExpr`).
#[derive(Debug, Clone)]
pub struct StringCharAtExpr {
    /// Zig `object: *Expr`.
    pub object: ExprId,
    /// Zig `index: *Expr`.
    pub index: ExprId,
    /// Zig `ty: ResolvedType = .integer`.
    pub ty: ResolvedType,
    /// Zig `span: Span`.
    pub span: Span,
}

/// `s.substring(start, end)` — half-open byte range as a fresh owned String
/// (Zig `StringSubstringExpr`).
#[derive(Debug, Clone)]
pub struct StringSubstringExpr {
    /// Zig `object: *Expr`.
    pub object: ExprId,
    /// Zig `start: *Expr`.
    pub start: ExprId,
    /// Zig `end: *Expr`.
    pub end: ExprId,
    /// Zig `ty: ResolvedType = .string`.
    pub ty: ResolvedType,
    /// Zig `span: Span`.
    pub span: Span,
}

/// `s.indexOf(needle)` — first byte offset or -1 (Zig `StringIndexOfExpr`).
#[derive(Debug, Clone)]
pub struct StringIndexOfExpr {
    /// Zig `object: *Expr`.
    pub object: ExprId,
    /// Zig `needle: *Expr`.
    pub needle: ExprId,
    /// Zig `ty: ResolvedType = .integer`.
    pub ty: ResolvedType,
    /// Zig `span: Span`.
    pub span: Span,
}

/// Field access (Zig `FieldExpr`).
#[derive(Debug, Clone)]
pub struct FieldExpr {
    /// Zig `object: *Expr`.
    pub object: ExprId,
    /// Zig `container_type_name: []const u8`.
    pub container_type_name: String,
    /// Zig `field_name: []const u8`.
    pub field_name: String,
    /// Zig `field_index: u32`.
    pub field_index: u32,
    /// Zig `ty: ResolvedType`.
    pub ty: ResolvedType,
    /// Zig `storage: FieldStorage`.
    pub storage: crate::hir::FieldStorage,
    /// Zig `span: Span`.
    pub span: Span,
    /// Zig `moved` — checker-classified field MOVE-OUT (`let p = obj.nodes`
    /// on a non-copyable field, KSEM107): codegen must null the field storage
    /// after the read.
    pub moved: bool,
}

/// `nativeState(...)` wrapper (Zig `NativeStateExpr`).
#[derive(Debug, Clone)]
pub struct NativeStateExpr {
    /// Zig `value: *Expr`.
    pub value: ExprId,
    /// Zig `ty: ResolvedType`.
    pub ty: ResolvedType,
    /// Zig `span: Span`.
    pub span: Span,
}

/// `nativeUserData(state)` (Zig `NativeUserDataExpr`).
#[derive(Debug, Clone)]
pub struct NativeUserDataExpr {
    /// Zig `state: *Expr`.
    pub state: ExprId,
    /// Zig `ty: ResolvedType = raw_ptr "RawPtr"`.
    pub ty: ResolvedType,
    /// Zig `span: Span`.
    pub span: Span,
}

/// `nativeRecover(...)` (Zig `NativeRecoverExpr`).
#[derive(Debug, Clone)]
pub struct NativeRecoverExpr {
    /// Zig `value: *Expr`.
    pub value: ExprId,
    /// Zig `ty: ResolvedType`.
    pub ty: ResolvedType,
    /// Zig `span: Span`.
    pub span: Span,
}

/// `nativeStateFree(state)` (Zig `NativeStateFreeExpr`).
#[derive(Debug, Clone)]
pub struct NativeStateFreeExpr {
    /// Zig `state: *Expr`.
    pub state: ExprId,
    /// Zig `ty: ResolvedType = .void`.
    pub ty: ResolvedType,
    /// Zig `span: Span`.
    pub span: Span,
}

/// Binary operator (Zig `BinaryOp`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BinaryOp {
    /// Zig `.add`.
    Add,
    /// Zig `.subtract`.
    Subtract,
    /// Zig `.multiply`.
    Multiply,
    /// Zig `.divide`.
    Divide,
    /// Zig `.modulo`.
    Modulo,
    /// Zig `.equal`.
    Equal,
    /// Zig `.not_equal`.
    NotEqual,
    /// Zig `.less`.
    Less,
    /// Zig `.less_equal`.
    LessEqual,
    /// Zig `.greater`.
    Greater,
    /// Zig `.greater_equal`.
    GreaterEqual,
    /// Zig `.logical_and`.
    LogicalAnd,
    /// Zig `.logical_or`.
    LogicalOr,
    /// Zig `.bit_and`.
    BitAnd,
    /// Zig `.bit_or`.
    BitOr,
    /// Zig `.bit_xor`.
    BitXor,
    /// Zig `.shift_left`.
    ShiftLeft,
    /// Zig `.shift_right`.
    ShiftRight,
}

/// Unary operator (Zig `UnaryOp`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UnaryOp {
    /// Zig `.negate`.
    Negate,
    /// Zig `.not`.
    Not,
    /// Zig `.bit_not`.
    BitNot,
}

/// Binary operation (Zig `BinaryExpr`).
#[derive(Debug, Clone)]
pub struct BinaryExpr {
    /// Zig `op: BinaryOp`.
    pub op: BinaryOp,
    /// Zig `lhs: *Expr`.
    pub lhs: ExprId,
    /// Zig `rhs: *Expr`.
    pub rhs: ExprId,
    /// Zig `ty: ResolvedType`.
    pub ty: ResolvedType,
    /// Zig `span: Span`.
    pub span: Span,
}

/// Unary operation (Zig `UnaryExpr`).
#[derive(Debug, Clone)]
pub struct UnaryExpr {
    /// Zig `op: UnaryOp`.
    pub op: UnaryOp,
    /// Zig `operand: *Expr`.
    pub operand: ExprId,
    /// Zig `ty: ResolvedType`.
    pub ty: ResolvedType,
    /// Zig `span: Span`.
    pub span: Span,
}

/// `Int(x)` / `Float(x)` numeric cast (Zig `CastExpr`).
#[derive(Debug, Clone)]
pub struct CastExpr {
    /// Zig `operand: *Expr`.
    pub operand: ExprId,
    /// Zig `ty: ResolvedType` — the destination numeric type.
    pub ty: ResolvedType,
    /// Zig `span: Span`.
    pub span: Span,
    /// Zig `reinterpret` — `floatToBits`/`bitsToFloat` bit reinterpretation.
    pub reinterpret: bool,
}

/// Ternary conditional (Zig `ConditionalExpr`).
#[derive(Debug, Clone)]
pub struct ConditionalExpr {
    /// Zig `condition: *Expr`.
    pub condition: ExprId,
    /// Zig `then_expr: *Expr`.
    pub then_expr: ExprId,
    /// Zig `else_expr: *Expr`.
    pub else_expr: ExprId,
    /// Zig `ty: ResolvedType`.
    pub ty: ResolvedType,
    /// Zig `span: Span`.
    pub span: Span,
}

/// How omitted construct fields are filled (Zig `ConstructFillMode`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ConstructFillMode {
    /// Zig `.defaults`.
    #[default]
    Defaults,
    /// Zig `.zeroed_ffi_c_layout`.
    ZeroedFfiCLayout,
}

/// Struct/construct literal (Zig `ConstructExpr`).
#[derive(Debug, Clone)]
pub struct ConstructExpr {
    /// Zig `type_name: []const u8`.
    pub type_name: String,
    /// Zig `fields: []ConstructFieldInit`.
    pub fields: Vec<ConstructFieldInit>,
    /// Zig `fill_mode: ConstructFillMode`.
    pub fill_mode: ConstructFillMode,
    /// Zig `ty: ResolvedType`.
    pub ty: ResolvedType,
    /// Zig `span: Span`.
    pub span: Span,
}

/// Enum variant construction (Zig `ConstructEnumVariantExpr`).
#[derive(Debug, Clone)]
pub struct ConstructEnumVariantExpr {
    /// Zig `enum_name: []const u8`.
    pub enum_name: String,
    /// Zig `variant_name: []const u8`.
    pub variant_name: String,
    /// Zig `discriminant: u32`.
    pub discriminant: u32,
    /// Zig `payload: ?*Expr`.
    pub payload: Option<ExprId>,
    /// Zig `ty: ResolvedType`.
    pub ty: ResolvedType,
    /// Zig `span: Span`.
    pub span: Span,
}

/// One field initializer in a construct literal (Zig `ConstructFieldInit`).
#[derive(Debug, Clone)]
pub struct ConstructFieldInit {
    /// Zig `field_name: ?[]const u8`.
    pub field_name: Option<String>,
    /// Zig `field_index: ?u32`.
    pub field_index: Option<u32>,
    /// Zig `value: *Expr`.
    pub value: ExprId,
    /// Zig `span: Span`.
    pub span: Span,
}

/// Direct call (Zig `CallExpr`).
#[derive(Debug, Clone)]
pub struct CallExpr {
    /// Zig `callee_name: []const u8`.
    pub callee_name: String,
    /// Zig `function_id: ?u32`.
    pub function_id: Option<u32>,
    /// Zig `args: []*Expr`.
    pub args: Vec<ExprId>,
    /// Zig `trailing_builder: ?BuilderBlock`.
    pub trailing_builder: Option<BuilderBlock>,
    /// Zig `ty: ResolvedType`.
    pub ty: ResolvedType,
    /// Zig `span: Span`.
    pub span: Span,
}

/// Virtual (family-dispatched) call (Zig `VirtualCallExpr`).
#[derive(Debug, Clone)]
pub struct VirtualCallExpr {
    /// Zig `receiver: *Expr`.
    pub receiver: ExprId,
    /// Zig `static_type_name: []const u8`.
    pub static_type_name: String,
    /// Zig `method_name: []const u8`.
    pub method_name: String,
    /// Zig `args: []*Expr`.
    pub args: Vec<ExprId>,
    /// Zig `ty: ResolvedType`.
    pub ty: ResolvedType,
    /// Zig `span: Span`.
    pub span: Span,
}

/// Closure literal (Zig `CallbackExpr`).
#[derive(Debug, Clone)]
pub struct CallbackExpr {
    /// Zig `params: []Parameter`.
    pub params: Vec<Parameter>,
    /// Zig `captures: []Capture`.
    pub captures: Vec<Capture>,
    /// Zig `locals: []LocalSymbol`.
    pub locals: Vec<LocalSymbol>,
    /// Zig `body: []Statement`.
    pub body: Vec<Statement>,
    /// Zig `return_type: ResolvedType`.
    pub return_type: ResolvedType,
    /// Zig `ty: ResolvedType`.
    pub ty: ResolvedType,
    /// Zig `span: Span`.
    pub span: Span,
}

/// One closure capture (Zig `Capture`).
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

/// Call through a callable value (Zig `CallValueExpr`).
#[derive(Debug, Clone)]
pub struct CallValueExpr {
    /// Zig `callee: *Expr`.
    pub callee: ExprId,
    /// Zig `args: []*Expr`.
    pub args: Vec<ExprId>,
    /// Zig `param_types: []const ResolvedType`.
    pub param_types: Vec<ResolvedType>,
    /// Zig `param_ownership` — per-parameter ownership of the callable's
    /// signature; empty means "treat all as owned".
    pub param_ownership: Vec<OwnershipMode>,
    /// Zig `ty: ResolvedType`.
    pub ty: ResolvedType,
    /// Zig `span: Span`.
    pub span: Span,
}

/// Array literal (Zig `ArrayExpr`).
#[derive(Debug, Clone)]
pub struct ArrayExpr {
    /// Zig `elements: []*Expr`.
    pub elements: Vec<ExprId>,
    /// Zig `ty: ResolvedType`.
    pub ty: ResolvedType,
    /// Zig `span: Span`.
    pub span: Span,
}

/// Builder-block array literal (Zig `BuilderArrayExpr`).
#[derive(Debug, Clone)]
pub struct BuilderArrayExpr {
    /// Zig `builder: BuilderBlock`.
    pub builder: BuilderBlock,
    /// Zig `ty: ResolvedType`.
    pub ty: ResolvedType,
    /// Zig `span: Span`.
    pub span: Span,
}

/// Array indexing (Zig `IndexExpr`).
#[derive(Debug, Clone)]
pub struct IndexExpr {
    /// Zig `object: *Expr`.
    pub object: ExprId,
    /// Zig `index: *Expr`.
    pub index: ExprId,
    /// Zig `ty: ResolvedType`.
    pub ty: ResolvedType,
    /// Zig `moved` — checker-verified element DRAIN: dst takes the element
    /// and the slot tombstones to VOID on every backend.
    pub moved: bool,
    /// Zig `span: Span`.
    pub span: Span,
}

/// Spawn a deferred call: runs when the task is first driven (Zig `TaskSpawnExpr`).
#[derive(Debug, Clone)]
pub struct TaskSpawnExpr {
    /// Zig `callee_name: []const u8`.
    pub callee_name: String,
    /// Zig `function_id: u32`.
    pub function_id: u32,
    /// Zig `args: []*Expr` (evaluated eagerly at the spawn site).
    pub args: Vec<ExprId>,
    /// Zig `ty` — the task's RESULT type (the handle stays checker-transparent).
    pub ty: ResolvedType,
    /// Zig `span: Span`.
    pub span: Span,
}

/// Spawn an already-completed task carrying a pure value (Zig `TaskSpawnReadyExpr`).
#[derive(Debug, Clone)]
pub struct TaskSpawnReadyExpr {
    /// Zig `value: *Expr`.
    pub value: ExprId,
    /// Zig `ty: ResolvedType`.
    pub ty: ResolvedType,
    /// Zig `span: Span`.
    pub span: Span,
}

/// Join a task; joining a cancelled task or joining twice traps (Zig `TaskAwaitExpr`).
#[derive(Debug, Clone)]
pub struct TaskAwaitExpr {
    /// Zig `task: *Expr`.
    pub task: ExprId,
    /// Zig `ty: ResolvedType`.
    pub ty: ResolvedType,
    /// Zig `span: Span`.
    pub span: Span,
}

/// Cooperative cancel; never force-terminates (Zig `TaskCancelExpr`).
#[derive(Debug, Clone)]
pub struct TaskCancelExpr {
    /// Zig `task: *Expr`.
    pub task: ExprId,
    /// Zig `span: Span`.
    pub span: Span,
}

/// Drive the task and discard the result (Zig `TaskDetachExpr`).
#[derive(Debug, Clone)]
pub struct TaskDetachExpr {
    /// Zig `task: *Expr`.
    pub task: ExprId,
    /// Zig `span: Span`.
    pub span: Span,
}

/// `taskYield()` cooperative progress point (Zig `TaskYieldExpr`).
#[derive(Debug, Clone)]
pub struct TaskYieldExpr {
    /// Zig `span: Span`.
    pub span: Span,
}

/// `taskSleep(ms)` — park for at least `ms` milliseconds (Zig `TaskSleepExpr`).
#[derive(Debug, Clone)]
pub struct TaskSleepExpr {
    /// Zig `milliseconds: *Expr`.
    pub milliseconds: ExprId,
    /// Zig `span: Span`.
    pub span: Span,
}

/// A builder block — construct/array content DSL (Zig `BuilderBlock`).
#[derive(Debug, Clone)]
pub struct BuilderBlock {
    /// Zig `items: []BuilderItem`.
    pub items: Vec<BuilderItem>,
    /// Zig `span: Span`.
    pub span: Span,
}

/// One item in a builder block (Zig `BuilderItem`).
#[derive(Debug, Clone)]
pub enum BuilderItem {
    /// Zig `.expr`.
    Expr(BuilderExprItem),
    /// Zig `.if_item`.
    If(BuilderIfItem),
    /// Zig `.for_item`.
    For(BuilderForItem),
    /// Zig `.switch_item`.
    Switch(BuilderSwitchItem),
}

/// Expression item in a builder block (Zig `BuilderExprItem`).
#[derive(Debug, Clone)]
pub struct BuilderExprItem {
    /// Zig `expr: *Expr`.
    pub expr: ExprId,
    /// Zig `span: Span`.
    pub span: Span,
}

/// Conditional builder item (Zig `BuilderIfItem`).
#[derive(Debug, Clone)]
pub struct BuilderIfItem {
    /// Zig `condition: *Expr`.
    pub condition: ExprId,
    /// Zig `then_block: BuilderBlock`.
    pub then_block: BuilderBlock,
    /// Zig `else_block: ?BuilderBlock`.
    pub else_block: Option<BuilderBlock>,
    /// Zig `span: Span`.
    pub span: Span,
}

/// Loop builder item (Zig `BuilderForItem`).
#[derive(Debug, Clone)]
pub struct BuilderForItem {
    /// Zig `binding_name: []const u8`.
    pub binding_name: String,
    /// Zig `binding_local_id: u32`.
    pub binding_local_id: u32,
    /// Zig `binding_ty: ResolvedType`.
    pub binding_ty: ResolvedType,
    /// Zig `iterator: *Expr`.
    pub iterator: ExprId,
    /// Zig `body: BuilderBlock`.
    pub body: BuilderBlock,
    /// Zig `span: Span`.
    pub span: Span,
}

/// Switch builder item (Zig `BuilderSwitchItem`).
#[derive(Debug, Clone)]
pub struct BuilderSwitchItem {
    /// Zig `subject: *Expr`.
    pub subject: ExprId,
    /// Zig `cases: []BuilderSwitchCase`.
    pub cases: Vec<BuilderSwitchCase>,
    /// Zig `default_block: ?BuilderBlock`.
    pub default_block: Option<BuilderBlock>,
    /// Zig `span: Span`.
    pub span: Span,
}

/// One case of a switch builder item (Zig `BuilderSwitchCase`).
#[derive(Debug, Clone)]
pub struct BuilderSwitchCase {
    /// Zig `pattern: *Expr`.
    pub pattern: ExprId,
    /// Zig `body: BuilderBlock`.
    pub body: BuilderBlock,
    /// Zig `span: Span`.
    pub span: Span,
}

// TODO(port): `exprType(expr) ResolvedType` — the type accessor over every
// Expr variant (kira-zig hir.zig `exprType`).
