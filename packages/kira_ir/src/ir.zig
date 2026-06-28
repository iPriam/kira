const runtime_abi = @import("kira_runtime_abi");

pub const ValueType = struct {
    kind: Kind,
    name: ?[]const u8 = null,
    construct_constraint: ?ConstructConstraint = null,

    pub const Kind = enum {
        void,
        integer,
        float,
        string,
        boolean,
        construct_any,
        array,
        raw_ptr,
        ffi_struct,
        enum_instance,
    };
};

pub const ConstructConstraint = struct {
    construct_name: []const u8,
};

pub const OwnershipMode = enum(u8) {
    owned,
    borrow_read,
    borrow_mut,
    move,
    copy,
};

pub const Program = struct {
    constructs: []Construct = &.{},
    construct_implementations: []ConstructImplementation = &.{},
    types: []TypeDecl = &.{},
    enums: []EnumTypeDecl = &.{},
    functions: []Function,
    entry_index: usize,
};

pub const Construct = struct {
    name: []const u8,
};

pub const ConstructImplementation = struct {
    type_name: []const u8,
    construct_constraint: ConstructConstraint,
    families: []const []const u8 = &.{},
    fields: []Field,
    has_content: bool,
    lifecycle_hooks: []LifecycleHook,
};

pub const LifecycleHook = struct {
    name: []const u8,
};

pub const TypeDecl = struct {
    name: []const u8,
    kind: TypeKind = .struct_decl,
    execution: runtime_abi.FunctionExecution = .inherited,
    fields: []Field,
    methods: []MethodMember = &.{},
    ffi: ?FfiTypeInfo = null,
};

pub const TypeKind = enum {
    class,
    struct_decl,
};

pub const MethodMember = struct {
    name: []const u8,
    function_id: u32,
    receiver_offset: u32,
};

pub const EnumTypeDecl = struct {
    name: []const u8,
    variants: []EnumVariantIr,
};

pub const EnumVariantIr = struct {
    name: []const u8,
    discriminant: u32,
    payload_ty: ?ValueType = null,
};

pub const Field = struct {
    name: []const u8,
    ty: ValueType,
};

pub const FfiTypeInfo = union(enum) {
    ffi_struct,
    pointer: PointerInfo,
    alias: AliasInfo,
    array: ArrayInfo,
    callback: CallbackInfo,
};

pub const PointerInfo = struct {
    target_name: []const u8,
};

pub const AliasInfo = struct {
    target: ValueType,
};

pub const ArrayInfo = struct {
    element: ValueType,
    count: usize,
};

pub const CallbackInfo = struct {
    params: []const ValueType,
    result: ValueType,
};

pub const ForeignFunction = struct {
    library_name: []const u8,
    symbol_name: []const u8,
    calling_convention: runtime_abi.CallingConvention = .c,
};

pub const Function = struct {
    id: u32,
    name: []const u8,
    execution: runtime_abi.FunctionExecution,
    is_extern: bool = false,
    foreign: ?ForeignFunction = null,
    param_types: []const ValueType = &.{},
    param_ownership: []const OwnershipMode = &.{},
    return_type: ValueType = .{ .kind = .void },
    return_ownership: OwnershipMode = .owned,
    register_count: u32,
    local_count: u32,
    local_types: []const ValueType,
    instructions: []Instruction,
};

pub const Instruction = union(enum) {
    const_int: ConstInt,
    const_float: ConstFloat,
    const_string: ConstString,
    const_bool: ConstBool,
    const_null_ptr: ConstNullPtr,
    const_function: ConstFunction,
    const_closure: ConstClosure,
    alloc_struct: AllocStruct,
    alloc_enum: AllocEnum,
    alloc_native_state: AllocNativeState,
    alloc_array: AllocArray,
    add: Binary,
    subtract: Binary,
    multiply: Binary,
    divide: Binary,
    modulo: Binary,
    convert: Convert,
    compare: Compare,
    unary: Unary,
    store_local: StoreLocal,
    load_local: LoadLocal,
    local_ptr: LocalPtr,
    subobject_ptr: SubobjectPtr,
    field_ptr: FieldPtr,
    recover_native_state: RecoverNativeState,
    native_state_field_get: NativeStateFieldGet,
    native_state_field_set: NativeStateFieldSet,
    c_string_to_string: CStringToString,
    array_len: ArrayLen,
    string_len: StringLen,
    array_get: ArrayGet,
    array_set: ArraySet,
    array_append: ArrayAppend,
    enum_tag: EnumTag,
    enum_payload: EnumPayload,
    load_indirect: LoadIndirect,
    store_indirect: StoreIndirect,
    copy_indirect: CopyIndirect,
    branch: Branch,
    jump: Jump,
    label: Label,
    print: Print,
    call: Call,
    call_virtual: VirtualCall,
    call_value: CallValue,
    ret: Return,
    // Scope markers for drop elaboration. `scope_enter` opens a droppable scope
    // (loop body); `scope_exit` closes it, dropping owned values created within the
    // scope (loop-body locals + register temporaries) at iteration end so they are
    // not leaked until function exit. LLVM-backend only; the VM/bytecode path treats
    // them as no-ops (the VM reclaims via its own native-layout destructors).
    scope_enter: ScopeEnter,
    scope_exit: ScopeExit,
};

pub const ScopeEnter = struct {};

pub const ScopeExit = struct {
    // Mapped IR local ids declared within the scope, to be dropped on scope exit.
    locals: []const u32,
};

pub const ConstInt = struct {
    dst: u32,
    value: i64,
};

pub const ConstFloat = struct {
    dst: u32,
    value: f64,
};

pub const ConstString = struct {
    dst: u32,
    value: []const u8,
};

pub const ConstBool = struct {
    dst: u32,
    value: bool,
};

pub const ConstNullPtr = struct {
    dst: u32,
};

pub const ConstFunction = struct {
    dst: u32,
    function_id: u32,
    representation: FunctionConstRepresentation = .callable_value,
};

pub const ConstClosure = struct {
    dst: u32,
    function_id: u32,
    captures: []const u32,
    capture_ownership: []const OwnershipMode = &.{},
};

pub const LocalPtr = struct {
    dst: u32,
    local: u32,
};

pub const FunctionConstRepresentation = enum {
    callable_value,
    native_callback,
};

pub const AllocStruct = struct {
    dst: u32,
    type_name: []const u8,
};

pub const AllocEnum = struct {
    dst: u32,
    enum_type_name: []const u8,
    discriminant: u32,
    payload_src: ?u32 = null,
};

pub const AllocNativeState = struct {
    dst: u32,
    src: u32,
    type_name: []const u8,
    type_id: u64,
};

pub const AllocArray = struct {
    dst: u32,
    len: u32,
    ty: ValueType = .{ .kind = .array },
};

pub const Binary = struct {
    dst: u32,
    lhs: u32,
    rhs: u32,
};

// Numeric conversion between Int and Float (the `Int(x)` / `Float(x)` cast
// surface). `target` is the destination numeric kind; the source kind is read
// from the operand register's inferred type (LLVM) or its runtime tag (VM).
// Float->Int truncates toward zero; Int->Float is an exact widening; a cast to
// the kind a value already has is an identity copy.
pub const Convert = struct {
    dst: u32,
    src: u32,
    target: ValueType.Kind,
};

pub const Compare = struct {
    dst: u32,
    lhs: u32,
    rhs: u32,
    op: CompareOp,
};

pub const CompareOp = enum {
    equal,
    not_equal,
    less,
    less_equal,
    greater,
    greater_equal,
};

pub const Unary = struct {
    dst: u32,
    src: u32,
    op: UnaryOp,
};

pub const UnaryOp = enum {
    negate,
    not,
};

pub const StoreLocal = struct {
    local: u32,
    src: u32,
    // Reborrow: bind the local as a non-owning alias of the source pointer
    // instead of cloning/owning it. Set when lowering `var r = t` where `t` is a
    // borrow (Rust reborrow semantics). The VM stores the slot borrowed (not freed
    // at frame exit); the LLVM backend's plain pointer store already aliases and its
    // drop pass leaves untracked (non-copy_indirect) locals alone.
    borrow: bool = false,
};

pub const LoadLocal = struct {
    dst: u32,
    local: u32,
    ownership: OwnershipMode = .borrow_read,
};

pub const SubobjectPtr = struct {
    dst: u32,
    base: u32,
    offset: u32,
};

pub const FieldPtr = struct {
    dst: u32,
    base: u32,
    base_type_name: []const u8,
    field_index: u32,
    field_ty: ValueType,
};

pub const RecoverNativeState = struct {
    dst: u32,
    state: u32,
    type_name: []const u8,
    type_id: u64,
};

pub const NativeStateFieldGet = struct {
    dst: u32,
    state: u32,
    field_index: u32,
    field_ty: ValueType,
};

pub const NativeStateFieldSet = struct {
    state: u32,
    field_index: u32,
    src: u32,
    field_ty: ValueType,
};

pub const CStringToString = struct {
    dst: u32,
    src: u32,
};

pub const ArrayLen = struct {
    dst: u32,
    array: u32,
};

pub const StringLen = struct {
    dst: u32,
    string: u32,
};

pub const ArrayGet = struct {
    dst: u32,
    array: u32,
    index: u32,
    ty: ValueType,
};

pub const ArraySet = struct {
    array: u32,
    index: u32,
    src: u32,
};

pub const ArrayAppend = struct {
    array: u32,
    src: u32,
};

pub const EnumTag = struct {
    dst: u32,
    src: u32,
};

pub const EnumPayload = struct {
    dst: u32,
    src: u32,
    payload_ty: ValueType,
};

pub const LoadIndirect = struct {
    dst: u32,
    ptr: u32,
    ty: ValueType,
};

pub const StoreIndirect = struct {
    ptr: u32,
    src: u32,
    ty: ValueType,
};

pub const CopyIndirect = struct {
    dst_ptr: u32,
    src_ptr: u32,
    type_name: []const u8,
};

pub const Branch = struct {
    condition: u32,
    true_label: u32,
    false_label: u32,
};

pub const Jump = struct {
    label: u32,
};

pub const Label = struct {
    id: u32,
};

pub const Print = struct {
    src: u32,
    ty: ValueType,
};

pub const Call = struct {
    callee: u32,
    args: []const u32,
    dst: ?u32 = null,
};

pub const VirtualCall = struct {
    receiver: u32,
    static_type_name: []const u8,
    method_name: []const u8,
    args: []const u32,
    return_ty: ValueType,
    dst: ?u32 = null,
};

pub const CallValue = struct {
    callee: u32,
    args: []const u32,
    param_types: []const ValueType,
    // Per-parameter ownership so the backend drop pass can escape arguments consumed by an
    // owned/move parameter (mirrors Function.param_ownership). Empty => treat all as owned.
    param_ownership: []const OwnershipMode = &.{},
    return_type: ValueType,
    dst: ?u32 = null,
};

pub const Return = struct {
    src: ?u32 = null,
};
