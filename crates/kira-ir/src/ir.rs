//! Low IR containers: programs, functions, and type declarations.
//!
//! Ported from kira-zig `kira_ir/src/ir.zig` (container half; the instruction
//! set lives in [`crate::instruction`]).

use kira_runtime_abi::{CallingConvention, FunctionExecution};
use kira_semantics_model::Span;

use crate::instruction::Instruction;

/// Kind of a low-IR value type (Zig `ValueType.Kind`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ValueTypeKind {
    /// Zig `.void`.
    #[default]
    Void,
    /// Zig `.integer`.
    Integer,
    /// Zig `.float`.
    Float,
    /// Zig `.string`.
    String,
    /// Zig `.boolean`.
    Boolean,
    /// Zig `.construct_any`.
    ConstructAny,
    /// Zig `.array`.
    Array,
    /// Zig `.raw_ptr`.
    RawPtr,
    /// Zig `.ffi_struct`.
    FfiStruct,
    /// Zig `.enum_instance`.
    EnumInstance,
}

/// A low-IR value type (Zig `ValueType`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct ValueType {
    /// Zig `kind: Kind`.
    pub kind: ValueTypeKind,
    /// Zig `name: ?[]const u8`.
    pub name: Option<String>,
    /// Zig `construct_constraint: ?ConstructConstraint`.
    pub construct_constraint: Option<ConstructConstraint>,
}

impl ValueType {
    /// A bare `void` value type (the Zig default `.{ .kind = .void }`).
    pub fn void() -> ValueType {
        ValueType::default()
    }
}

/// A construct constraint on a value type (Zig `ConstructConstraint`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ConstructConstraint {
    /// Zig `construct_name: []const u8`.
    pub construct_name: String,
}

/// How a value crosses a binding/call boundary (Zig `OwnershipMode`, `enum(u8)`).
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum OwnershipMode {
    /// Zig `.owned = 0`.
    #[default]
    Owned = 0,
    /// Zig `.borrow_read = 1`.
    BorrowRead = 1,
    /// Zig `.borrow_mut = 2`.
    BorrowMut = 2,
    /// Zig `.move = 3`.
    Move = 3,
    /// Zig `.copy = 4`.
    Copy = 4,
}

/// A lowered low-IR program (Zig `Program`).
#[derive(Debug, Clone, Default)]
pub struct Program {
    /// Zig `constructs: []Construct`.
    pub constructs: Vec<Construct>,
    /// Zig `construct_implementations: []ConstructImplementation`.
    pub construct_implementations: Vec<ConstructImplementation>,
    /// Zig `types: []TypeDecl`.
    pub types: Vec<TypeDecl>,
    /// Zig `enums: []EnumTypeDecl`.
    pub enums: Vec<EnumTypeDecl>,
    /// Zig `functions: []Function`.
    pub functions: Vec<Function>,
    /// Zig `entry_index: usize`.
    pub entry_index: usize,
}

/// A construct declaration (Zig `Construct`).
#[derive(Debug, Clone, PartialEq)]
pub struct Construct {
    /// Zig `name: []const u8`.
    pub name: String,
}

/// A construct implementation (Zig `ConstructImplementation`).
#[derive(Debug, Clone)]
pub struct ConstructImplementation {
    /// Zig `type_name: []const u8`.
    pub type_name: String,
    /// Zig `construct_constraint: ConstructConstraint`.
    pub construct_constraint: ConstructConstraint,
    /// Zig `families: []const []const u8`.
    pub families: Vec<String>,
    /// Zig `fields: []Field`.
    pub fields: Vec<Field>,
    /// Zig `has_content: bool`.
    pub has_content: bool,
    /// Zig `lifecycle_hooks: []LifecycleHook`.
    pub lifecycle_hooks: Vec<LifecycleHook>,
}

/// A lifecycle hook name (Zig `LifecycleHook`).
#[derive(Debug, Clone, PartialEq)]
pub struct LifecycleHook {
    /// Zig `name: []const u8`.
    pub name: String,
}

/// Kind of a type declaration (Zig `TypeKind`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum TypeKind {
    /// Zig `.class`.
    Class,
    /// Zig `.struct_decl`.
    #[default]
    StructDecl,
}

/// A struct/class declaration (Zig `TypeDecl`).
#[derive(Debug, Clone)]
pub struct TypeDecl {
    /// Zig `name: []const u8`.
    pub name: String,
    /// Zig `kind: TypeKind = .struct_decl`.
    pub kind: TypeKind,
    /// Zig `execution: FunctionExecution = .inherited`.
    pub execution: FunctionExecution,
    /// Zig `fields: []Field`.
    pub fields: Vec<Field>,
    /// Zig `methods: []MethodMember`.
    pub methods: Vec<MethodMember>,
    /// Zig `ffi: ?FfiTypeInfo`.
    pub ffi: Option<FfiTypeInfo>,
}

/// A method table entry (Zig `MethodMember`).
#[derive(Debug, Clone, PartialEq)]
pub struct MethodMember {
    /// Zig `name: []const u8`.
    pub name: String,
    /// Zig `function_id: u32`.
    pub function_id: u32,
    /// Zig `receiver_offset: u32`.
    pub receiver_offset: u32,
}

/// An enum declaration (Zig `EnumTypeDecl`).
#[derive(Debug, Clone)]
pub struct EnumTypeDecl {
    /// Zig `name: []const u8`.
    pub name: String,
    /// Zig `variants: []EnumVariantIr`.
    pub variants: Vec<EnumVariantIr>,
}

/// An enum variant (Zig `EnumVariantIr`).
#[derive(Debug, Clone)]
pub struct EnumVariantIr {
    /// Zig `name: []const u8`.
    pub name: String,
    /// Zig `discriminant: u32`.
    pub discriminant: u32,
    /// Zig `payload_ty: ?ValueType`.
    pub payload_ty: Option<ValueType>,
}

/// A named, typed field (Zig `Field`).
#[derive(Debug, Clone)]
pub struct Field {
    /// Zig `name: []const u8`.
    pub name: String,
    /// Zig `ty: ValueType`.
    pub ty: ValueType,
}

/// FFI metadata on a type declaration (Zig `FfiTypeInfo`).
#[derive(Debug, Clone)]
pub enum FfiTypeInfo {
    /// Zig `.ffi_struct`.
    FfiStruct,
    /// Zig `.pointer`.
    Pointer(PointerInfo),
    /// Zig `.alias`.
    Alias(AliasInfo),
    /// Zig `.array`.
    Array(ArrayInfo),
    /// Zig `.callback`.
    Callback(CallbackInfo),
}

/// FFI pointer info (Zig `PointerInfo`).
#[derive(Debug, Clone)]
pub struct PointerInfo {
    /// Zig `target_name: []const u8`.
    pub target_name: String,
}

/// FFI alias info (Zig `AliasInfo`).
#[derive(Debug, Clone)]
pub struct AliasInfo {
    /// Zig `target: ValueType`.
    pub target: ValueType,
}

/// FFI fixed-size array info (Zig `ArrayInfo`).
#[derive(Debug, Clone)]
pub struct ArrayInfo {
    /// Zig `element: ValueType`.
    pub element: ValueType,
    /// Zig `count: usize`.
    pub count: usize,
}

/// FFI callback signature info (Zig `CallbackInfo`).
#[derive(Debug, Clone)]
pub struct CallbackInfo {
    /// Zig `params: []const ValueType`.
    pub params: Vec<ValueType>,
    /// Zig `result: ValueType`.
    pub result: ValueType,
}

/// Foreign (FFI) binding (Zig `ForeignFunction`).
#[derive(Debug, Clone, PartialEq)]
pub struct ForeignFunction {
    /// Zig `library_name: []const u8`.
    pub library_name: String,
    /// Zig `symbol_name: []const u8`.
    pub symbol_name: String,
    /// Zig `calling_convention: CallingConvention = .c`.
    pub calling_convention: CallingConvention,
}

/// A low-IR function (Zig `Function`).
#[derive(Debug, Clone, Default)]
pub struct Function {
    /// Zig `id: u32`.
    pub id: u32,
    /// Zig `name: []const u8`.
    pub name: String,
    /// Zig `is_async: bool = false`.
    pub is_async: bool,
    /// Zig `execution: FunctionExecution`.
    pub execution: FunctionExecution,
    /// Zig `is_extern: bool = false`.
    pub is_extern: bool,
    /// Zig `foreign: ?ForeignFunction`.
    pub foreign: Option<ForeignFunction>,
    /// Zig `param_types: []const ValueType`.
    pub param_types: Vec<ValueType>,
    /// Zig `param_ownership: []const OwnershipMode`.
    pub param_ownership: Vec<OwnershipMode>,
    /// Zig `return_type: ValueType = .{ .kind = .void }`.
    pub return_type: ValueType,
    /// Zig `return_ownership: OwnershipMode = .owned`.
    pub return_ownership: OwnershipMode,
    /// Zig `register_count: u32`.
    pub register_count: u32,
    /// Zig `local_count: u32`.
    pub local_count: u32,
    /// Zig `local_types: []const ValueType`.
    pub local_types: Vec<ValueType>,
    /// Zig `local_names` — source-level names per local slot for the
    /// debugger's variables view; empty when lowered without name info.
    pub local_names: Vec<String>,
    /// Zig `instructions: []Instruction`.
    pub instructions: Vec<Instruction>,
    /// Zig `locations` — source span per instruction, index-aligned with
    /// `instructions` when populated; the debugger's line-table source of
    /// truth in low IR. `idx >= locations.len` and `{0,0}` mean "no location".
    pub locations: Vec<Span>,
}

/// Task-frame layout constant (Zig `frame_state_slot`): slot 0 = resume state.
pub const FRAME_STATE_SLOT: u32 = 0;
/// Task-frame layout constant (Zig `frame_result_slot`): slot 1 = return value.
pub const FRAME_RESULT_SLOT: u32 = 1;
/// Task-frame layout constant (Zig `frame_first_data_slot`): slots 2.. = params then locals.
pub const FRAME_FIRST_DATA_SLOT: u32 = 2;
/// Status returned by a transformed suspendable body (Zig `task_status_complete`).
pub const TASK_STATUS_COMPLETE: i64 = 0;
/// Status returned by a transformed suspendable body (Zig `task_status_suspended`).
pub const TASK_STATUS_SUSPENDED: i64 = 1;
