//! Decoded KBC instructions (opcode + payload).
//!
//! Ported from kira-zig `kira_bytecode/src/instruction.zig` (`Instruction`, a
//! union over `OpCode`). Variant order matches the opcode order exactly; see
//! [`crate::opcode::OpCode`] for the append-only discipline.

use crate::ownership_mode::OwnershipMode;

/// Selects the byte format for a scalar -> String conversion (`String(x)`)
/// (Zig `StringFromScalarSource`, `enum(u8)`).
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StringFromScalarSource {
    /// Zig `.integer = 0`.
    Integer = 0,
    /// Zig `.float = 1`.
    Float = 1,
    /// Zig `.boolean = 2`.
    Boolean = 2,
}

/// Arithmetic kind carried by the fused superinstructions (Zig `ArithKind`, `enum(u8)`).
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ArithKind {
    /// Zig `.add = 0`.
    Add = 0,
    /// Zig `.subtract = 1`.
    Subtract = 1,
    /// Zig `.multiply = 2`.
    Multiply = 2,
}

/// How a `const_function` result is represented (Zig `FunctionConstRepresentation`, `enum(u8)`).
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum FunctionConstRepresentation {
    /// Zig `.callable_value = 0`.
    #[default]
    CallableValue = 0,
    /// Zig `.native_callback = 1`.
    NativeCallback = 1,
}

/// Comparison predicate (Zig `CompareOp`, `enum(u8)`).
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CompareOp {
    /// Zig `.equal = 0`.
    Equal = 0,
    /// Zig `.not_equal = 1`.
    NotEqual = 1,
    /// Zig `.less = 2`.
    Less = 2,
    /// Zig `.less_equal = 3`.
    LessEqual = 3,
    /// Zig `.greater = 4`.
    Greater = 4,
    /// Zig `.greater_equal = 5`.
    GreaterEqual = 5,
}

/// Unary operator (Zig `UnaryOp`, `enum(u8)`).
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UnaryOp {
    /// Zig `.negate = 0`.
    Negate = 0,
    /// Zig `.not = 1`.
    Not = 1,
    /// Zig `.bit_not = 2`.
    BitNot = 2,
}

/// Bitwise/shift operator (Zig `BitOp`, `enum(u8)`).
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BitOp {
    /// Zig `.bit_and = 0`.
    BitAnd = 0,
    /// Zig `.bit_or = 1`.
    BitOr = 1,
    /// Zig `.bit_xor = 2`.
    BitXor = 2,
    /// Zig `.shift_left = 3`.
    ShiftLeft = 3,
    /// Zig `.shift_right = 4`.
    ShiftRight = 4,
}

/// Construct constraint carried on a [`TypeRef`] (Zig `TypeRef.ConstructConstraint`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ConstructConstraint {
    /// Zig `construct_name: []const u8`.
    pub construct_name: String,
}

/// Kind of a [`TypeRef`] (Zig `TypeRef.Kind`, `enum(u8)`).
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum TypeRefKind {
    /// Zig `.void = 0`.
    #[default]
    Void = 0,
    /// Zig `.integer = 1`.
    Integer = 1,
    /// Zig `.float = 2`.
    Float = 2,
    /// Zig `.string = 3`.
    String = 3,
    /// Zig `.boolean = 4`.
    Boolean = 4,
    /// Zig `.construct_any = 5`.
    ConstructAny = 5,
    /// Zig `.array = 6`.
    Array = 6,
    /// Zig `.raw_ptr = 7`.
    RawPtr = 7,
    /// Zig `.ffi_struct = 8`.
    FfiStruct = 8,
    /// Zig `.enum_instance = 9`.
    EnumInstance = 9,
}

/// Serialized type reference carried on typed instructions (Zig `TypeRef`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct TypeRef {
    /// Zig `kind: Kind`.
    pub kind: TypeRefKind,
    /// Zig `name: ?[]const u8` — precise type name (e.g. FFI primitive names).
    pub name: Option<String>,
    /// Zig `construct_constraint: ?ConstructConstraint`.
    pub construct_constraint: Option<ConstructConstraint>,
}

impl TypeRef {
    /// A bare `void` type reference (the Zig default `.{ .kind = .void }`).
    pub fn void() -> TypeRef {
        TypeRef::default()
    }
}

/// A decoded instruction: opcode + payload (Zig `Instruction`, `union(OpCode)`).
///
/// Zig `[]const u8` string payloads are owned `String`s here and `[]const u32`
/// register lists are `Vec<u32>` (no lifetimes in model types).
#[derive(Debug, Clone, PartialEq)]
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
    AllocArray { dst: u32, len: u32 },
    /// Zig `.add`.
    Add { dst: u32, lhs: u32, rhs: u32 },
    /// Zig `.subtract`.
    Subtract { dst: u32, lhs: u32, rhs: u32 },
    /// Zig `.multiply`.
    Multiply { dst: u32, lhs: u32, rhs: u32 },
    /// Zig `.divide` — `unsigned` selects unsigned division (KBC8+).
    Divide {
        dst: u32,
        lhs: u32,
        rhs: u32,
        unsigned: bool,
    },
    /// Zig `.modulo` — `unsigned` selects unsigned remainder (KBC8+).
    Modulo {
        dst: u32,
        lhs: u32,
        rhs: u32,
        unsigned: bool,
    },
    /// Zig `.compare` — `unsigned` selects unsigned ordering (KBC8+).
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
    /// (Rust reborrow semantics).
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
        field_ty: TypeRef,
    },
    /// Zig `.recover_native_state`.
    RecoverNativeState {
        dst: u32,
        state: u32,
        type_name: String,
        type_id: u64,
    },
    /// Zig `.native_state_field_get` — `moved` marks a checker-verified
    /// move-out: the VM takes ownership into `dst` and VOIDS the payload slot
    /// (KBCA+).
    NativeStateFieldGet {
        dst: u32,
        state: u32,
        field_index: u32,
        field_ty: TypeRef,
        moved: bool,
    },
    /// Zig `.native_state_field_set`.
    NativeStateFieldSet {
        state: u32,
        field_index: u32,
        src: u32,
        field_ty: TypeRef,
    },
    /// Zig `.c_string_to_string`.
    CStringToString { dst: u32, src: u32 },
    /// Zig `.array_len`.
    ArrayLen { dst: u32, array: u32 },
    /// Zig `.string_len`.
    StringLen { dst: u32, string: u32 },
    /// Zig `.array_get` — `borrow` aliases a managed element for a
    /// non-escaping read; `moved` is a checker-verified element drain (KBCA+).
    ArrayGet {
        dst: u32,
        array: u32,
        index: u32,
        ty: TypeRef,
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
        payload_ty: TypeRef,
    },
    /// Zig `.load_indirect` — `moved` marks a checker-verified field move-out:
    /// the VM takes ownership and VOIDS the field slot (Rust partial move, KBCA+).
    LoadIndirect {
        dst: u32,
        ptr: u32,
        ty: TypeRef,
        moved: bool,
    },
    /// Zig `.store_indirect`.
    StoreIndirect { ptr: u32, src: u32, ty: TypeRef },
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
    Print { src: u32, ty: TypeRef },
    /// Zig `.call_runtime`.
    CallRuntime {
        function_id: u32,
        args: Vec<u32>,
        dst: Option<u32>,
    },
    /// Zig `.call_native`.
    CallNative {
        function_id: u32,
        args: Vec<u32>,
        dst: Option<u32>,
        return_ty: TypeRef,
    },
    /// Zig `.call_virtual`.
    CallVirtual {
        receiver: u32,
        static_type_name: String,
        method_name: String,
        args: Vec<u32>,
        return_ty: TypeRef,
        dst: Option<u32>,
    },
    /// Zig `.call_value`.
    CallValue {
        callee: u32,
        args: Vec<u32>,
        param_ownership: Vec<OwnershipMode>,
        dst: Option<u32>,
    },
    /// Zig `.ret`.
    Ret { src: Option<u32> },
    /// Zig `.convert` — numeric Int<->Float cast; `to_float` selects the
    /// target; `reinterpret` is a Float<->bits bitcast (KBCB+).
    Convert {
        dst: u32,
        src: u32,
        to_float: bool,
        reinterpret: bool,
    },
    /// Zig `.bitwise`.
    Bitwise {
        dst: u32,
        lhs: u32,
        rhs: u32,
        op: BitOp,
        unsigned: bool,
    },
    /// Zig `.free_native_state`.
    FreeNativeState { state: u32 },
    /// Zig `.task_spawn` — `native` dispatches through the VM's native-call
    /// hook at first drive (hybrid); `suspendable` allocates a `frame_slots`
    /// state-machine frame and drives by status.
    TaskSpawn {
        dst: u32,
        callee: u32,
        args: Vec<u32>,
        result_ty: TypeRef,
        native: bool,
        suspendable: bool,
        frame_slots: u32,
    },
    /// Zig `.task_spawn_ready`.
    TaskSpawnReady { dst: u32, value: u32, ty: TypeRef },
    /// Zig `.task_await`.
    TaskAwait { dst: u32, task: u32, ty: TypeRef },
    /// Zig `.task_cancel`.
    TaskCancel { task: u32 },
    /// Zig `.task_detach`.
    TaskDetach { task: u32 },
    /// Zig `.task_yield`.
    TaskYield,
    /// Zig `.frame_get`.
    FrameGet {
        dst: u32,
        frame: u32,
        slot: u32,
        ty: TypeRef,
    },
    /// Zig `.frame_set`.
    FrameSet {
        frame: u32,
        slot: u32,
        src: u32,
        ty: TypeRef,
    },
    /// Zig `.task_is_complete`.
    TaskIsComplete { dst: u32, task: u32 },
    /// Zig `.task_sleep`.
    TaskSleep { milliseconds: u32 },
    /// Zig `.string_from_scalar` — `String(x)`; `source` picks the byte format.
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
    // --- VM-internal fused forms (never serialized); see OpCode docs. ------
    /// Zig `.fused_compare_branch` — compare + branch with a pattern-private dst.
    FusedCompareBranch {
        lhs: u32,
        rhs: u32,
        op: CompareOp,
        true_target: u32,
        false_target: u32,
    },
    /// Zig `.fused_compare_const_branch` — const_int + compare + branch.
    FusedCompareConstBranch {
        lhs: u32,
        imm: i64,
        op: CompareOp,
        true_target: u32,
        false_target: u32,
    },
    /// Zig `.fused_cmp_local_const_branch` — load_local + const_int + compare + branch.
    FusedCmpLocalConstBranch {
        local: u32,
        imm: i64,
        op: CompareOp,
        true_target: u32,
        false_target: u32,
    },
    /// Zig `.fused_arith_locals_store` — load, load, arith, store_local.
    FusedArithLocalsStore {
        kind: ArithKind,
        lhs_local: u32,
        rhs_local: u32,
        dst_local: u32,
    },
    /// Zig `.fused_arith_local_const_store` — load, const_int, arith, store_local.
    FusedArithLocalConstStore {
        kind: ArithKind,
        lhs_local: u32,
        imm: i64,
        dst_local: u32,
    },
    /// Zig `.fused_arith_locals_ret` — the entire body of a leaf arithmetic function.
    FusedArithLocalsRet {
        kind: ArithKind,
        lhs_local: u32,
        rhs_local: u32,
    },
    /// Zig `.fused_array_bind_local` — the `for x in array` element binding
    /// (aliases the element instead of deep-cloning it).
    FusedArrayBindLocal {
        array: u32,
        index: u32,
        dst_local: u32,
        type_name: String,
    },
    /// Zig `.fused_array_field_load` — reads one scalar field of an array
    /// element (`arr[i].f`) in one step.
    FusedArrayFieldLoad {
        dst: u32,
        array: u32,
        index: u32,
        elem_ty: TypeRef,
        field_index: u32,
    },
}

impl Instruction {
    /// Returns the [`crate::opcode::OpCode`] for this instruction
    /// (Zig `std.meta.activeTag`).
    pub fn opcode(&self) -> crate::opcode::OpCode {
        use crate::opcode::OpCode;
        match self {
            Instruction::ConstInt { .. } => OpCode::ConstInt,
            Instruction::ConstFloat { .. } => OpCode::ConstFloat,
            Instruction::ConstString { .. } => OpCode::ConstString,
            Instruction::ConstBool { .. } => OpCode::ConstBool,
            Instruction::ConstNullPtr { .. } => OpCode::ConstNullPtr,
            Instruction::ConstFunction { .. } => OpCode::ConstFunction,
            Instruction::ConstClosure { .. } => OpCode::ConstClosure,
            Instruction::AllocStruct { .. } => OpCode::AllocStruct,
            Instruction::AllocEnum { .. } => OpCode::AllocEnum,
            Instruction::AllocNativeState { .. } => OpCode::AllocNativeState,
            Instruction::AllocArray { .. } => OpCode::AllocArray,
            Instruction::Add { .. } => OpCode::Add,
            Instruction::Subtract { .. } => OpCode::Subtract,
            Instruction::Multiply { .. } => OpCode::Multiply,
            Instruction::Divide { .. } => OpCode::Divide,
            Instruction::Modulo { .. } => OpCode::Modulo,
            Instruction::Compare { .. } => OpCode::Compare,
            Instruction::Unary { .. } => OpCode::Unary,
            Instruction::StoreLocal { .. } => OpCode::StoreLocal,
            Instruction::LoadLocal { .. } => OpCode::LoadLocal,
            Instruction::LocalPtr { .. } => OpCode::LocalPtr,
            Instruction::SubobjectPtr { .. } => OpCode::SubobjectPtr,
            Instruction::FieldPtr { .. } => OpCode::FieldPtr,
            Instruction::RecoverNativeState { .. } => OpCode::RecoverNativeState,
            Instruction::NativeStateFieldGet { .. } => OpCode::NativeStateFieldGet,
            Instruction::NativeStateFieldSet { .. } => OpCode::NativeStateFieldSet,
            Instruction::CStringToString { .. } => OpCode::CStringToString,
            Instruction::ArrayLen { .. } => OpCode::ArrayLen,
            Instruction::StringLen { .. } => OpCode::StringLen,
            Instruction::ArrayGet { .. } => OpCode::ArrayGet,
            Instruction::ArraySet { .. } => OpCode::ArraySet,
            Instruction::ArrayAppend { .. } => OpCode::ArrayAppend,
            Instruction::EnumTag { .. } => OpCode::EnumTag,
            Instruction::EnumPayload { .. } => OpCode::EnumPayload,
            Instruction::LoadIndirect { .. } => OpCode::LoadIndirect,
            Instruction::StoreIndirect { .. } => OpCode::StoreIndirect,
            Instruction::CopyIndirect { .. } => OpCode::CopyIndirect,
            Instruction::Branch { .. } => OpCode::Branch,
            Instruction::Jump { .. } => OpCode::Jump,
            Instruction::Label { .. } => OpCode::Label,
            Instruction::Print { .. } => OpCode::Print,
            Instruction::CallRuntime { .. } => OpCode::CallRuntime,
            Instruction::CallNative { .. } => OpCode::CallNative,
            Instruction::CallVirtual { .. } => OpCode::CallVirtual,
            Instruction::CallValue { .. } => OpCode::CallValue,
            Instruction::Ret { .. } => OpCode::Ret,
            Instruction::Convert { .. } => OpCode::Convert,
            Instruction::Bitwise { .. } => OpCode::Bitwise,
            Instruction::FreeNativeState { .. } => OpCode::FreeNativeState,
            Instruction::TaskSpawn { .. } => OpCode::TaskSpawn,
            Instruction::TaskSpawnReady { .. } => OpCode::TaskSpawnReady,
            Instruction::TaskAwait { .. } => OpCode::TaskAwait,
            Instruction::TaskCancel { .. } => OpCode::TaskCancel,
            Instruction::TaskDetach { .. } => OpCode::TaskDetach,
            Instruction::TaskYield => OpCode::TaskYield,
            Instruction::FrameGet { .. } => OpCode::FrameGet,
            Instruction::FrameSet { .. } => OpCode::FrameSet,
            Instruction::TaskIsComplete { .. } => OpCode::TaskIsComplete,
            Instruction::TaskSleep { .. } => OpCode::TaskSleep,
            Instruction::StringFromScalar { .. } => OpCode::StringFromScalar,
            Instruction::StringCharAt { .. } => OpCode::StringCharAt,
            Instruction::StringSubstring { .. } => OpCode::StringSubstring,
            Instruction::StringIndexOf { .. } => OpCode::StringIndexOf,
            Instruction::FusedCompareBranch { .. } => OpCode::FusedCompareBranch,
            Instruction::FusedCompareConstBranch { .. } => OpCode::FusedCompareConstBranch,
            Instruction::FusedCmpLocalConstBranch { .. } => OpCode::FusedCmpLocalConstBranch,
            Instruction::FusedArithLocalsStore { .. } => OpCode::FusedArithLocalsStore,
            Instruction::FusedArithLocalConstStore { .. } => OpCode::FusedArithLocalConstStore,
            Instruction::FusedArithLocalsRet { .. } => OpCode::FusedArithLocalsRet,
            Instruction::FusedArrayBindLocal { .. } => OpCode::FusedArrayBindLocal,
            Instruction::FusedArrayFieldLoad { .. } => OpCode::FusedArrayFieldLoad,
        }
    }
}
