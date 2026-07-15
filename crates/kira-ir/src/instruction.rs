//! The low-IR instruction set (register machine).
//!
//! Ported from kira-zig `kira_ir/src/ir.zig` (`Instruction`, `union(enum)`).
//! Unlike the bytecode opcode set, low-IR tags are not serialized, so there
//! is no discriminant-stability constraint here.

use crate::ir::{OwnershipMode, ValueType, ValueTypeKind};

/// Comparison predicate (Zig `CompareOp`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CompareOp {
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

/// Bitwise/shift operator (Zig `BitOp`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BitOp {
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

/// How a `const_function` result is represented (Zig `FunctionConstRepresentation`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FunctionConstRepresentation {
    /// Zig `.callable_value`.
    #[default]
    CallableValue,
    /// Zig `.native_callback`.
    NativeCallback,
}

/// Byte format of a `String(x)` conversion (Zig `StringFromScalarSource`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StringFromScalarSource {
    /// Zig `.integer`.
    Integer,
    /// Zig `.float`.
    Float,
    /// Zig `.boolean`.
    Boolean,
}

/// A low-IR instruction (Zig `Instruction`).
///
/// Registers are plain `u32` ids; `[]const u32` register lists are `Vec<u32>`
/// and `[]const u8` names are owned `String`s.
#[derive(Debug, Clone)]
pub enum Instruction {
    /// Zig `.const_int`.
    ConstInt { dst: u32, value: i64 },
    /// Zig `.const_float`.
    ConstFloat { dst: u32, value: f64 },
    /// Zig `.const_string`.
    ConstString { dst: u32, value: String },
    /// Zig `.const_bool`.
    ConstBool { dst: u32, value: bool },
    /// Zig `.const_null_ptr`.
    ConstNullPtr { dst: u32 },
    /// Zig `.const_function`.
    ConstFunction {
        dst: u32,
        function_id: u32,
        representation: FunctionConstRepresentation,
    },
    /// Zig `.const_closure`.
    ConstClosure {
        dst: u32,
        function_id: u32,
        captures: Vec<u32>,
        capture_ownership: Vec<OwnershipMode>,
    },
    /// Zig `.alloc_struct`.
    AllocStruct { dst: u32, type_name: String },
    /// Zig `.alloc_enum`.
    AllocEnum {
        dst: u32,
        enum_type_name: String,
        discriminant: u32,
        payload_src: Option<u32>,
    },
    /// Zig `.alloc_native_state`.
    AllocNativeState {
        dst: u32,
        src: u32,
        type_name: String,
        type_id: u64,
    },
    /// Zig `.alloc_array`.
    AllocArray { dst: u32, len: u32, ty: ValueType },
    /// Zig `.add`.
    Add { dst: u32, lhs: u32, rhs: u32 },
    /// Zig `.subtract`.
    Subtract { dst: u32, lhs: u32, rhs: u32 },
    /// Zig `.multiply`.
    Multiply { dst: u32, lhs: u32, rhs: u32 },
    /// Zig `.divide` — `unsigned` selects unsigned division (U8..U64 operands).
    Divide {
        dst: u32,
        lhs: u32,
        rhs: u32,
        unsigned: bool,
    },
    /// Zig `.modulo` — `unsigned` selects unsigned remainder.
    Modulo {
        dst: u32,
        lhs: u32,
        rhs: u32,
        unsigned: bool,
    },
    /// Zig `.bitwise` — `unsigned` only matters for `shift_right` (logical vs
    /// arithmetic).
    Bitwise {
        dst: u32,
        lhs: u32,
        rhs: u32,
        op: BitOp,
        unsigned: bool,
    },
    /// Zig `.convert` — numeric conversion between Int and Float; `target` is
    /// the destination kind; `reinterpret` keeps the bit pattern (Float<->bits).
    Convert {
        dst: u32,
        src: u32,
        target: ValueTypeKind,
        reinterpret: bool,
    },
    /// Zig `.compare` — `unsigned` selects unsigned ordering predicates.
    Compare {
        dst: u32,
        lhs: u32,
        rhs: u32,
        op: CompareOp,
        unsigned: bool,
    },
    /// Zig `.unary`.
    Unary { dst: u32, src: u32, op: UnaryOp },
    /// Zig `.store_local` — `borrow` binds the local as a non-owning alias
    /// (Rust reborrow).
    StoreLocal { local: u32, src: u32, borrow: bool },
    /// Zig `.load_local`.
    LoadLocal {
        dst: u32,
        local: u32,
        ownership: OwnershipMode,
    },
    /// Zig `.local_ptr`.
    LocalPtr { dst: u32, local: u32 },
    /// Zig `.subobject_ptr`.
    SubobjectPtr { dst: u32, base: u32, offset: u32 },
    /// Zig `.field_ptr`.
    FieldPtr {
        dst: u32,
        base: u32,
        base_type_name: String,
        field_index: u32,
        field_ty: ValueType,
    },
    /// Zig `.recover_native_state`.
    RecoverNativeState {
        dst: u32,
        state: u32,
        type_name: String,
        type_id: u64,
    },
    /// Zig `.free_native_state`.
    FreeNativeState { state: u32 },
    /// Zig `.native_state_field_get` — `moved` nulls the payload slot after
    /// the read (field move-out).
    NativeStateFieldGet {
        dst: u32,
        state: u32,
        field_index: u32,
        field_ty: ValueType,
        moved: bool,
    },
    /// Zig `.native_state_field_set`.
    NativeStateFieldSet {
        state: u32,
        field_index: u32,
        src: u32,
        field_ty: ValueType,
    },
    /// Zig `.c_string_to_string`.
    CStringToString { dst: u32, src: u32 },
    /// Zig `.array_len`.
    ArrayLen { dst: u32, array: u32 },
    /// Zig `.string_len`.
    StringLen { dst: u32, string: u32 },
    /// Zig `.string_from_scalar` — `String(x)`.
    StringFromScalar {
        dst: u32,
        src: u32,
        source: StringFromScalarSource,
    },
    /// Zig `.string_char_at`.
    StringCharAt { dst: u32, string: u32, index: u32 },
    /// Zig `.string_substring`.
    StringSubstring {
        dst: u32,
        string: u32,
        start: u32,
        end: u32,
    },
    /// Zig `.string_index_of`.
    StringIndexOf { dst: u32, string: u32, needle: u32 },
    /// Zig `.array_get` — `borrow` aliases a managed element for a
    /// non-escaping read; `moved` is a checker-verified element drain
    /// (the slot tombstones to VOID).
    ArrayGet {
        dst: u32,
        array: u32,
        index: u32,
        ty: ValueType,
        borrow: bool,
        moved: bool,
    },
    /// Zig `.array_set`.
    ArraySet { array: u32, index: u32, src: u32 },
    /// Zig `.array_append`.
    ArrayAppend { array: u32, src: u32 },
    /// Zig `.enum_tag`.
    EnumTag { dst: u32, src: u32 },
    /// Zig `.enum_payload`.
    EnumPayload {
        dst: u32,
        src: u32,
        payload_ty: ValueType,
    },
    /// Zig `.load_indirect` — `moved` is a checker-verified field MOVE-OUT:
    /// ownership transfers to `dst` and the backend nulls the field storage.
    LoadIndirect {
        dst: u32,
        ptr: u32,
        ty: ValueType,
        moved: bool,
    },
    /// Zig `.store_indirect`.
    StoreIndirect { ptr: u32, src: u32, ty: ValueType },
    /// Zig `.copy_indirect`.
    CopyIndirect {
        dst_ptr: u32,
        src_ptr: u32,
        type_name: String,
    },
    /// Zig `.branch`.
    Branch {
        condition: u32,
        true_label: u32,
        false_label: u32,
    },
    /// Zig `.jump`.
    Jump { label: u32 },
    /// Zig `.label`.
    Label { id: u32 },
    /// Zig `.print`.
    Print { src: u32, ty: ValueType },
    /// Zig `.call` — direct call by register-held callee id.
    Call {
        callee: u32,
        args: Vec<u32>,
        dst: Option<u32>,
    },
    /// Zig `.call_virtual`.
    CallVirtual {
        receiver: u32,
        static_type_name: String,
        method_name: String,
        args: Vec<u32>,
        return_ty: ValueType,
        dst: Option<u32>,
    },
    /// Zig `.call_value` — call through a callable value; `param_ownership`
    /// lets the backend drop pass escape arguments consumed by an owned/move
    /// parameter (empty => treat all as owned).
    CallValue {
        callee: u32,
        args: Vec<u32>,
        param_types: Vec<ValueType>,
        param_ownership: Vec<OwnershipMode>,
        return_type: ValueType,
        dst: Option<u32>,
    },
    /// Zig `.ret`.
    Ret { src: Option<u32> },
    /// Zig `.task_spawn` — captures callee + eagerly-evaluated args into a
    /// task WITHOUT calling; `suspendable` bodies get a `frame_slots` heap
    /// frame and are driven by status.
    TaskSpawn {
        dst: u32,
        callee: u32,
        args: Vec<u32>,
        result_ty: ValueType,
        suspendable: bool,
        frame_slots: u32,
    },
    /// Zig `.task_spawn_ready` — wraps a pure value as a completed task.
    TaskSpawnReady { dst: u32, value: u32, ty: ValueType },
    /// Zig `.task_await` — joins the task and yields the result.
    TaskAwait { dst: u32, task: u32, ty: ValueType },
    /// Zig `.task_cancel` — sets the cooperative flag.
    TaskCancel { task: u32 },
    /// Zig `.task_detach` — drives and discards.
    TaskDetach { task: u32 },
    /// Zig `.task_yield` — cooperative progress point.
    TaskYield,
    /// Zig `.frame_get` — task-frame slot read for suspendable bodies.
    FrameGet {
        dst: u32,
        frame: u32,
        slot: u32,
        ty: ValueType,
    },
    /// Zig `.frame_set` — task-frame slot write.
    FrameSet {
        frame: u32,
        slot: u32,
        src: u32,
        ty: ValueType,
    },
    /// Zig `.task_is_complete` — true when the task is no longer pending.
    TaskIsComplete { dst: u32, task: u32 },
    /// Zig `.task_sleep` — park for at least `milliseconds`.
    TaskSleep { milliseconds: u32 },
    /// Zig `.scope_enter` — opens a droppable scope (loop body) for drop
    /// elaboration; LLVM-backend only, no-op on the VM path.
    ScopeEnter,
    /// Zig `.scope_exit` — closes the scope, dropping owned values created
    /// within it (`locals` = mapped IR local ids declared in the scope).
    ScopeExit { locals: Vec<u32> },
}
