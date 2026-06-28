const source_pkg = @import("kira_source");
const model = @import("kira_semantics_model");
const runtime_abi = @import("kira_runtime_abi");

pub const Program = struct {
    source_program: model.Program,
    functions: []Function,
    entry_index: usize,
};

pub const CheckedProgram = struct {
    program: Program,
};

pub const Function = struct {
    id: u32,
    name: []const u8,
    execution: runtime_abi.FunctionExecution,
    is_extern: bool = false,
    params: []const Parameter,
    locals: []const Local,
    captures: []const Capture = &.{},
    return_type: model.ResolvedType,
    return_ownership: model.OwnershipMode = .owned,
    body: Block,
    span: source_pkg.Span,
};

pub const Parameter = struct {
    id: u32,
    name: []const u8,
    ty: model.ResolvedType,
    ownership: model.OwnershipMode = .owned,
    span: source_pkg.Span,
};

pub const Local = struct {
    id: u32,
    name: []const u8,
    ty: model.ResolvedType,
    ownership: model.OwnershipMode = .owned,
    is_parameter: bool = false,
    is_capture: bool = false,
    span: source_pkg.Span,
};

pub const Capture = struct {
    local_id: u32,
    source_local_id: u32,
    by_ref: bool = false,
    ownership: model.OwnershipMode = .borrow_read,
    name: []const u8,
    ty: model.ResolvedType,
    span: source_pkg.Span,
};

pub const Block = struct {
    statements: []Statement,
    span: source_pkg.Span,
};

pub const Statement = union(enum) {
    let_stmt: LetStatement,
    assign_stmt: AssignStatement,
    expr_stmt: ExprStatement,
    if_stmt: IfStatement,
    for_stmt: ForStatement,
    while_stmt: WhileStatement,
    break_stmt: BreakStatement,
    continue_stmt: ContinueStatement,
    match_stmt: MatchStatement,
    switch_stmt: SwitchStatement,
    return_stmt: ReturnStatement,
};

pub const LetStatement = struct {
    local: Local,
    value: ?Value,
    is_reborrow: bool = false,
    span: source_pkg.Span,
};

pub const AssignStatement = struct {
    target: Place,
    value: Value,
    span: source_pkg.Span,
};

pub const ExprStatement = struct {
    value: Value,
    span: source_pkg.Span,
};

pub const IfStatement = struct {
    condition: Value,
    then_block: Block,
    else_block: ?Block,
    span: source_pkg.Span,
};

pub const ForStatement = struct {
    binding: Local,
    iterator: Value,
    body: Block,
    span: source_pkg.Span,
};

pub const WhileStatement = struct {
    condition: Value,
    body: Block,
    span: source_pkg.Span,
};

pub const BreakStatement = struct {
    span: source_pkg.Span,
};

pub const ContinueStatement = struct {
    span: source_pkg.Span,
};

pub const MatchStatement = struct {
    subject: Value,
    arms: []MatchArm,
    span: source_pkg.Span,
};

pub const MatchArm = struct {
    bound_locals: []const Local = &.{},
    guard: ?Value = null,
    body: Block,
    span: source_pkg.Span,
};

pub const SwitchStatement = struct {
    subject: Value,
    cases: []SwitchCase,
    default_block: ?Block,
    span: source_pkg.Span,
};

pub const SwitchCase = struct {
    pattern: Value,
    body: Block,
    span: source_pkg.Span,
};

pub const ReturnStatement = struct {
    return_place: Place,
    value: ?Value,
    span: source_pkg.Span,
};

pub const Value = union(enum) {
    integer: IntegerValue,
    float: FloatValue,
    string: StringValue,
    boolean: BooleanValue,
    null_ptr: NullPtrValue,
    function_ref: FunctionRefValue,
    place: PlaceValue,
    namespace_ref: NamespaceRefValue,
    call: CallValue,
    virtual_call: VirtualCallValue,
    callback: CallbackValue,
    call_value: CallValueExpr,
    construct: ConstructValue,
    construct_enum_variant: ConstructEnumVariantValue,
    array: ArrayValue,
    builder_array: BuilderArrayValue,
    binary: BinaryValue,
    unary: UnaryValue,
    cast: UnaryValue,
    conditional: ConditionalValue,
    native_state: UnaryWrapperValue,
    native_user_data: UnaryWrapperValue,
    native_recover: UnaryWrapperValue,
    c_string_to_string: UnaryWrapperValue,
    array_len: UnaryWrapperValue,
    string_len: UnaryWrapperValue,
    opaque_member: OpaqueMemberValue,
    opaque_index: OpaqueIndexValue,
};

pub const IntegerValue = struct {
    ty: model.ResolvedType,
    span: source_pkg.Span,
};

pub const FloatValue = struct {
    ty: model.ResolvedType,
    span: source_pkg.Span,
};

pub const StringValue = struct {
    ty: model.ResolvedType,
    span: source_pkg.Span,
};

pub const BooleanValue = struct {
    ty: model.ResolvedType,
    span: source_pkg.Span,
};

pub const NullPtrValue = struct {
    ty: model.ResolvedType,
    span: source_pkg.Span,
};

pub const FunctionRefValue = struct {
    function_id: u32,
    name: []const u8,
    ty: model.ResolvedType,
    span: source_pkg.Span,
};

pub const NamespaceRefValue = struct {
    path: []const u8,
    ty: model.ResolvedType,
    span: source_pkg.Span,
};

pub const PlaceValue = struct {
    place: Place,
    ownership: model.OwnershipMode = .borrow_read,
};

pub const CallValue = struct {
    callee_name: []const u8,
    function_id: ?u32 = null,
    args: []Value,
    param_ownership: []const model.OwnershipMode = &.{},
    return_ownership: model.OwnershipMode = .owned,
    ty: model.ResolvedType,
    temp_id: u32,
    span: source_pkg.Span,
};

pub const VirtualCallValue = struct {
    receiver: *Value,
    receiver_ownership: model.OwnershipMode = .borrow_read,
    static_type_name: []const u8,
    method_name: []const u8,
    args: []Value,
    param_ownership: []const model.OwnershipMode = &.{},
    return_ownership: model.OwnershipMode = .owned,
    ty: model.ResolvedType,
    temp_id: u32,
    span: source_pkg.Span,
};

pub const CallbackValue = struct {
    function_id: u32,
    captures: []const Capture,
    ty: model.ResolvedType,
    temp_id: u32,
    span: source_pkg.Span,
};

pub const CallValueExpr = struct {
    callee: *Value,
    args: []Value,
    param_ownership: []const model.OwnershipMode = &.{},
    return_ownership: model.OwnershipMode = .owned,
    ty: model.ResolvedType,
    temp_id: u32,
    span: source_pkg.Span,
};

pub const ConstructValue = struct {
    type_name: []const u8,
    fields: []ConstructFieldInit,
    ty: model.ResolvedType,
    temp_id: u32,
    span: source_pkg.Span,
};

pub const ConstructEnumVariantValue = struct {
    enum_name: []const u8,
    variant_name: []const u8,
    payload: ?*Value = null,
    ty: model.ResolvedType,
    temp_id: u32,
    span: source_pkg.Span,
};

pub const ConstructFieldInit = struct {
    field_name: ?[]const u8 = null,
    field_index: ?u32 = null,
    value: Value,
    span: source_pkg.Span,
};

pub const ArrayValue = struct {
    elements: []Value,
    ty: model.ResolvedType,
    temp_id: u32,
    span: source_pkg.Span,
};

pub const BuilderArrayValue = struct {
    builder: BuilderBlock,
    ty: model.ResolvedType,
    temp_id: u32,
    span: source_pkg.Span,
};

pub const BinaryValue = struct {
    lhs: *Value,
    rhs: *Value,
    ty: model.ResolvedType,
    temp_id: u32,
    span: source_pkg.Span,
};

pub const UnaryValue = struct {
    operand: *Value,
    ty: model.ResolvedType,
    temp_id: u32,
    span: source_pkg.Span,
};

pub const ConditionalValue = struct {
    condition: *Value,
    then_value: *Value,
    else_value: *Value,
    ty: model.ResolvedType,
    temp_id: u32,
    span: source_pkg.Span,
};

pub const UnaryWrapperValue = struct {
    inner: *Value,
    ty: model.ResolvedType,
    temp_id: u32,
    span: source_pkg.Span,
};

pub const OpaqueMemberValue = struct {
    object: *Value,
    field_name: []const u8,
    ty: model.ResolvedType,
    temp_id: u32,
    span: source_pkg.Span,
};

pub const OpaqueIndexValue = struct {
    object: *Value,
    index: *Value,
    ty: model.ResolvedType,
    temp_id: u32,
    span: source_pkg.Span,
};

pub const BuilderBlock = struct {
    items: []BuilderItem,
    span: source_pkg.Span,
};

pub const BuilderItem = union(enum) {
    expr: BuilderExprItem,
    if_item: BuilderIfItem,
    for_item: BuilderForItem,
    switch_item: BuilderSwitchItem,
};

pub const BuilderExprItem = struct {
    value: Value,
    span: source_pkg.Span,
};

pub const BuilderIfItem = struct {
    condition: Value,
    then_block: BuilderBlock,
    else_block: ?BuilderBlock,
    span: source_pkg.Span,
};

pub const BuilderForItem = struct {
    binding: Local,
    iterator: Value,
    body: BuilderBlock,
    span: source_pkg.Span,
};

pub const BuilderSwitchItem = struct {
    subject: Value,
    cases: []BuilderSwitchCase,
    default_block: ?BuilderBlock,
    span: source_pkg.Span,
};

pub const BuilderSwitchCase = struct {
    pattern: Value,
    body: BuilderBlock,
    span: source_pkg.Span,
};

pub const Place = struct {
    root: Root,
    projections: []Projection = &.{},
    ty: model.ResolvedType,
    span: source_pkg.Span,

    pub const Root = union(enum) {
        local: u32,
        capture: u32,
        return_slot,
    };
};

pub const Projection = union(enum) {
    field: FieldProjection,
    index: IndexProjection,
    parent_view: ParentViewProjection,
};

pub const FieldProjection = struct {
    container_type_name: []const u8,
    field_name: []const u8,
    field_index: u32,
    ty: model.ResolvedType,
    span: source_pkg.Span,
};

pub const IndexProjection = struct {
    index: ?i64 = null,
    dynamic_index: ?*Value = null,
    ty: model.ResolvedType,
    span: source_pkg.Span,
};

pub const ParentViewProjection = struct {
    offset: u32,
    ty: model.ResolvedType,
    span: source_pkg.Span,
};
