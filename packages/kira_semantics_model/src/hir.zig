const source_pkg = @import("kira_source");
const runtime_abi = @import("kira_runtime_abi");
const symbols = @import("symbols.zig");
const Type = @import("types.zig").Type;
const ResolvedType = @import("types.zig").ResolvedType;
const ConstructConstraint = @import("types.zig").ConstructConstraint;
const OwnershipMode = @import("types.zig").OwnershipMode;
const ffi = @import("ffi.zig");

pub const Program = struct {
    imports: []Import,
    annotations: []AnnotationDecl,
    capabilities: []CapabilityDecl = &.{},
    enums: []EnumDecl = &.{},
    constructs: []Construct,
    types: []TypeDecl,
    forms: []ConstructForm,
    tests: []TestCase = &.{},
    functions: []Function,
    entry_index: usize,
};

pub const Import = struct {
    module_name: []const u8,
    alias: ?[]const u8,
    package_name: ?[]const u8 = null,
    source_path: []const u8 = "",
    span: source_pkg.Span,
};

pub const Annotation = struct {
    name: []const u8,
    is_namespaced: bool = false,
    symbol_index: ?usize = null,
    arguments: []AnnotationArgument = &.{},
    span: source_pkg.Span,
};

pub const AnnotationDecl = struct {
    name: []const u8,
    targets: []AnnotationTarget = &.{},
    uses: []const []const u8 = &.{},
    generated_functions: []GeneratedFunction = &.{},
    parameters: []AnnotationParameterDecl,
    module_path: []const u8 = "",
    span: source_pkg.Span,
};

pub const CapabilityDecl = struct {
    name: []const u8,
    generated_functions: []GeneratedFunction = &.{},
    module_path: []const u8 = "",
    span: source_pkg.Span,
};

pub const EnumDecl = struct {
    name: []const u8,
    type_params: [][]const u8 = &.{},
    variants: []EnumVariantHir,
    span: source_pkg.Span,
};

pub const EnumVariantHir = struct {
    name: []const u8,
    discriminant: u32,
    payload_ty: ?ResolvedType = null,
    default_value: ?*Expr = null,
    span: source_pkg.Span,
};

pub const AnnotationTarget = enum {
    class,
    struct_decl,
    function,
    construct,
    field,
};

pub const GeneratedFunction = struct {
    name: []const u8,
    overridable: bool,
    params: []const ResolvedType = &.{},
    param_ownership: []const OwnershipMode = &.{},
    return_type: ResolvedType = .{ .kind = .unknown },
    return_ownership: OwnershipMode = .owned,
    source_annotation: []const u8 = "",
    span: source_pkg.Span,
};

pub const AnnotationParameterDecl = struct {
    name: []const u8,
    ty: ResolvedType,
    default_value: ?AnnotationValue = null,
    span: source_pkg.Span,
};

pub const AnnotationArgument = struct {
    name: []const u8,
    value: AnnotationValue,
    ty: ResolvedType,
    span: source_pkg.Span,
};

pub const AnnotationValue = union(enum) {
    integer: i64,
    float: f64,
    boolean: bool,
    string: []const u8,
};

pub const Construct = struct {
    name: []const u8,
    parents: []ConstructParent = &.{},
    properties: []PropertySchema = &.{},
    content_channels: []ContentChannel = &.{},
    content_refine: []ContentChannel = &.{},
    content_projections: []ContentProjection = &.{},
    content_sealed: bool = false,
    content_passthrough: bool = false,
    required_functions: []RequiredFunction = &.{},
    section_functions: []SectionFunction = &.{},
    // Fields a construct requires its concrete declarations to provide, declared as direct
    // `@Required let name: T` members (the SwiftUI-style surface).
    required_fields: []RequiredField = &.{},
    // Non-required direct members the construct supplies a default body for (e.g. the default
    // `let node: Node { body.node }`). A concrete declaration may override them, and overriding
    // every default member that consumes a required field discharges that requirement
    // (the terminal-`node` rule).
    default_members: []ConstructDefaultMember = &.{},
    allowed_annotations: []AnnotationRule,
    // Element type of a typed `content: Content<T>;` section, e.g. "Widget".
    // Null when the construct does not pin a content element type. When set,
    // construct-backed declarations are validated so their content block only
    // holds element-typed values. Content-requiredness is expressed through
    // content channels (`content { name { count 1.. } }`), not here.
    content_element_type: ?[]const u8 = null,
    allowed_lifecycle_hooks: [][]const u8,
    span: source_pkg.Span,
};

pub const SectionFunction = struct {
    name: []const u8,
    required: bool,
    param_types: []const []const u8,
    return_type: []const u8,
    span: source_pkg.Span,
};

pub const AnnotationRule = struct {
    name: []const u8,
    span: source_pkg.Span,
};

pub const ConstructParent = struct {
    name: []const u8,
    span: source_pkg.Span,
};

// A typed slot declared in a construct's `properties { ... }` schema. `type_text` is the
// canonical type text (e.g. "String", "Int", "UUID") used to validate the values supplied
// by construct-backed declarations.
pub const PropertySchema = struct {
    required: bool,
    name: []const u8,
    type_text: []const u8,
    span: source_pkg.Span,
};

// A function a construct requires its first concrete declaration to implement. `param_types`
// and `return_type` are canonical type texts (with `Self` left as the literal "Self", resolved
// against the implementing declaration during satisfaction checking).
pub const RequiredFunction = struct {
    name: []const u8,
    param_types: []const []const u8,
    return_type: []const u8,
    span: source_pkg.Span,
};

// A field a construct requires its concrete declarations to provide, declared as a direct
// `@Required let name: T` member. `type_text` is the canonical type text (with `Self` left
// literal). A required field whose `type_text` equals the construct's own name is recursive
// (e.g. `body: Widget` in `construct Widget`); such fields participate in the terminal rule.
pub const RequiredField = struct {
    name: []const u8,
    type_text: []const u8,
    span: source_pkg.Span,
};

// A non-required direct member with a default body. `references` lists the names of the
// construct's required fields read by the default body, so the satisfaction checker knows
// which requirements an override of this member discharges.
pub const ConstructDefaultMember = struct {
    name: []const u8,
    is_field: bool,
    references: []const []const u8,
    span: source_pkg.Span,
};

// A named content channel on a construct. `accepts` is the canonical element type text a
// channel accepts (null = unconstrained); `min`/`max` bound how many elements a declaration
// may place in the channel (max null = unbounded).
pub const ContentChannel = struct {
    name: []const u8,
    accepts: ?[]const u8,
    min: u32,
    max: ?u32,
    span: source_pkg.Span,
};

// A `content project { local as Parent.channel }` mapping resolved on a construct.
pub const ContentProjection = struct {
    local: []const u8,
    target_construct: []const u8,
    target_channel: []const u8,
    span: source_pkg.Span,
};

pub const TypeDecl = struct {
    kind: TypeKind = .struct_decl,
    name: []const u8,
    execution: runtime_abi.FunctionExecution = .inherited,
    fields: []const Field,
    methods: []const MethodMember = &.{},
    ffi: ?ffi.NamedTypeInfo = null,
    span: source_pkg.Span,
};

pub const TypeKind = enum {
    class,
    struct_decl,
};

pub const MethodMember = struct {
    name: []const u8,
    full_name: []const u8,
    receiver_offset: u32,
    span: source_pkg.Span,
};

pub const ConstructForm = struct {
    construct: ConstructConstraint,
    name: []const u8,
    families: []const []const u8 = &.{},
    fields: []const Field,
    content: ?BuilderBlock,
    lifecycle_hooks: []LifecycleHook,
    span: source_pkg.Span,
};

pub const TestCase = struct {
    name: []const u8,
    test_function: []const u8,
    expect_function: []const u8,
    result_type: ResolvedType,
    span: source_pkg.Span,
};

pub const Field = struct {
    name: []const u8,
    owner_type_name: []const u8,
    storage: FieldStorage,
    slot_index: u32,
    ty: ResolvedType,
    explicit_type: bool,
    default_value: ?*Expr = null,
    annotations: []Annotation,
    span: source_pkg.Span,
};

pub const LifecycleHook = struct {
    name: []const u8,
    span: source_pkg.Span,
};

pub const Function = struct {
    id: u32,
    name: []const u8,
    is_main: bool,
    execution: runtime_abi.FunctionExecution,
    is_extern: bool = false,
    foreign: ?ffi.ForeignFunction = null,
    annotations: []Annotation,
    params: []Parameter,
    locals: []symbols.LocalSymbol,
    return_type: ResolvedType,
    return_ownership: OwnershipMode = .owned,
    body: []Statement,
    span: source_pkg.Span,
};

pub const Parameter = struct {
    id: u32,
    name: []const u8,
    ty: ResolvedType,
    ownership: OwnershipMode = .owned,
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
    local_id: u32,
    ty: ResolvedType,
    explicit_type: bool,
    value: ?*Expr,
    // Rust reborrow: the initializer is a read of a borrow binding, so the new
    // local aliases the same storage rather than copying it. Backends bind it as
    // a non-owning alias instead of cloning the value.
    is_reborrow: bool = false,
    span: source_pkg.Span,
};

pub const ExprStatement = struct {
    expr: *Expr,
    span: source_pkg.Span,
};

pub const AssignStatement = struct {
    target: *Expr,
    value: *Expr,
    span: source_pkg.Span,
};

pub const IfStatement = struct {
    condition: *Expr,
    then_body: []Statement,
    else_body: ?[]Statement,
    span: source_pkg.Span,
};

pub const ForStatement = struct {
    binding_name: []const u8,
    binding_local_id: u32,
    binding_ty: ResolvedType,
    iterator: *Expr,
    body: []Statement,
    span: source_pkg.Span,
};

pub const WhileStatement = struct {
    condition: *Expr,
    body: []Statement,
    span: source_pkg.Span,
};

pub const BreakStatement = struct {
    span: source_pkg.Span,
};

pub const ContinueStatement = struct {
    span: source_pkg.Span,
};

pub const MatchStatement = struct {
    subject: *Expr,
    arms: []MatchArm,
    enum_name: []const u8,
    span: source_pkg.Span,
};

pub const MatchArm = struct {
    pattern: MatchPattern,
    guard: ?*Expr,
    body: []Statement,
    span: source_pkg.Span,
};

pub const MatchPattern = union(enum) {
    variant: VariantMatchPattern,
    binding: BindingMatchPattern,
};

pub const VariantMatchPattern = struct {
    variant_name: []const u8,
    discriminant: u32,
    payload_ty: ?ResolvedType = null,
    inner: ?*MatchPattern = null,
    as_binding_local_id: ?u32 = null,
    as_binding_ty: ?ResolvedType = null,
    span: source_pkg.Span,
};

pub const BindingMatchPattern = struct {
    local_id: u32,
    name: []const u8,
    ty: ResolvedType,
    span: source_pkg.Span,
};

pub const SwitchStatement = struct {
    subject: *Expr,
    cases: []SwitchCase,
    default_body: ?[]Statement,
    span: source_pkg.Span,
};

pub const SwitchCase = struct {
    pattern: *Expr,
    body: []Statement,
    span: source_pkg.Span,
};

pub const ReturnStatement = struct {
    value: ?*Expr,
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
    expr: *Expr,
    span: source_pkg.Span,
};

pub const BuilderIfItem = struct {
    condition: *Expr,
    then_block: BuilderBlock,
    else_block: ?BuilderBlock,
    span: source_pkg.Span,
};

pub const BuilderForItem = struct {
    binding_name: []const u8,
    binding_local_id: u32,
    binding_ty: ResolvedType,
    iterator: *Expr,
    body: BuilderBlock,
    span: source_pkg.Span,
};

pub const BuilderSwitchItem = struct {
    subject: *Expr,
    cases: []BuilderSwitchCase,
    default_block: ?BuilderBlock,
    span: source_pkg.Span,
};

pub const BuilderSwitchCase = struct {
    pattern: *Expr,
    body: BuilderBlock,
    span: source_pkg.Span,
};

pub const Expr = union(enum) {
    integer: IntegerExpr,
    float: FloatExpr,
    string: StringExpr,
    boolean: BooleanExpr,
    null_ptr: NullPtrExpr,
    function_ref: FunctionRefExpr,
    callback: CallbackExpr,
    local: LocalExpr,
    namespace_ref: NamespaceRefExpr,
    parent_view: ParentViewExpr,
    c_string_to_string: CStringToStringExpr,
    array_len: ArrayLenExpr,
    string_len: StringLenExpr,
    field: FieldExpr,
    native_state: NativeStateExpr,
    native_user_data: NativeUserDataExpr,
    native_recover: NativeRecoverExpr,
    binary: BinaryExpr,
    unary: UnaryExpr,
    cast: CastExpr,
    conditional: ConditionalExpr,
    construct: ConstructExpr,
    construct_enum_variant: ConstructEnumVariantExpr,
    call: CallExpr,
    virtual_call: VirtualCallExpr,
    call_value: CallValueExpr,
    array: ArrayExpr,
    builder_array: BuilderArrayExpr,
    index: IndexExpr,
};

pub const IntegerExpr = struct {
    value: i64,
    ty: ResolvedType = .{ .kind = .integer },
    span: source_pkg.Span,
};

pub const FloatExpr = struct {
    value: f64,
    ty: ResolvedType = .{ .kind = .float },
    span: source_pkg.Span,
};

pub const StringExpr = struct {
    value: []const u8,
    ty: ResolvedType = .{ .kind = .string },
    span: source_pkg.Span,
};

pub const BooleanExpr = struct {
    value: bool,
    ty: ResolvedType = .{ .kind = .boolean },
    span: source_pkg.Span,
};

pub const NullPtrExpr = struct {
    ty: ResolvedType,
    span: source_pkg.Span,
};

pub const FunctionRefExpr = struct {
    representation: FunctionRefRepresentation = .callable_value,
    function_id: u32,
    name: []const u8,
    ty: ResolvedType,
    span: source_pkg.Span,
};

pub const FunctionRefRepresentation = enum {
    callable_value,
    native_callback,
};

pub const LocalExpr = struct {
    local_id: u32,
    name: []const u8,
    ty: ResolvedType,
    storage: FieldStorage,
    ownership: OwnershipMode = .borrow_read,
    span: source_pkg.Span,
};

pub const NamespaceRefExpr = struct {
    root: []const u8,
    path: []const u8,
    ty: ResolvedType = .{ .kind = .unknown },
    span: source_pkg.Span,
};

pub const ParentViewExpr = struct {
    object: *Expr,
    ty: ResolvedType,
    offset: u32,
    span: source_pkg.Span,
};

pub const CStringToStringExpr = struct {
    value: *Expr,
    ty: ResolvedType = .{ .kind = .string },
    span: source_pkg.Span,
};

pub const ArrayLenExpr = struct {
    object: *Expr,
    ty: ResolvedType = .{ .kind = .integer },
    span: source_pkg.Span,
};

pub const StringLenExpr = struct {
    object: *Expr,
    ty: ResolvedType = .{ .kind = .integer },
    span: source_pkg.Span,
};

pub const FieldExpr = struct {
    object: *Expr,
    container_type_name: []const u8,
    field_name: []const u8,
    field_index: u32,
    ty: ResolvedType,
    storage: FieldStorage,
    span: source_pkg.Span,
};

pub const NativeStateExpr = struct {
    value: *Expr,
    ty: ResolvedType,
    span: source_pkg.Span,
};

pub const NativeUserDataExpr = struct {
    state: *Expr,
    ty: ResolvedType = .{ .kind = .raw_ptr, .name = "RawPtr" },
    span: source_pkg.Span,
};

pub const NativeRecoverExpr = struct {
    value: *Expr,
    ty: ResolvedType,
    span: source_pkg.Span,
};

pub const BinaryExpr = struct {
    op: BinaryOp,
    lhs: *Expr,
    rhs: *Expr,
    ty: ResolvedType,
    span: source_pkg.Span,
};

pub const UnaryExpr = struct {
    op: UnaryOp,
    operand: *Expr,
    ty: ResolvedType,
    span: source_pkg.Span,
};

// `Int(x)` / `Float(x)` numeric cast. `ty` is the destination numeric type
// (integer or float); `operand` is the (already type-checked numeric) source.
pub const CastExpr = struct {
    operand: *Expr,
    ty: ResolvedType,
    span: source_pkg.Span,
};

pub const ConditionalExpr = struct {
    condition: *Expr,
    then_expr: *Expr,
    else_expr: *Expr,
    ty: ResolvedType,
    span: source_pkg.Span,
};

pub const ConstructExpr = struct {
    type_name: []const u8,
    fields: []ConstructFieldInit,
    fill_mode: ConstructFillMode,
    ty: ResolvedType,
    span: source_pkg.Span,
};

pub const ConstructEnumVariantExpr = struct {
    enum_name: []const u8,
    variant_name: []const u8,
    discriminant: u32,
    payload: ?*Expr,
    ty: ResolvedType,
    span: source_pkg.Span,
};

pub const ConstructFieldInit = struct {
    field_name: ?[]const u8 = null,
    field_index: ?u32 = null,
    value: *Expr,
    span: source_pkg.Span,
};

pub const ConstructFillMode = enum {
    defaults,
    zeroed_ffi_c_layout,
};

pub const CallExpr = struct {
    callee_name: []const u8,
    function_id: ?u32,
    args: []*Expr,
    trailing_builder: ?BuilderBlock = null,
    ty: ResolvedType,
    span: source_pkg.Span,
};

pub const VirtualCallExpr = struct {
    receiver: *Expr,
    static_type_name: []const u8,
    method_name: []const u8,
    args: []*Expr,
    ty: ResolvedType,
    span: source_pkg.Span,
};

pub const CallbackExpr = struct {
    params: []Parameter,
    captures: []Capture,
    locals: []symbols.LocalSymbol,
    body: []Statement,
    return_type: ResolvedType,
    ty: ResolvedType,
    span: source_pkg.Span,
};

pub const Capture = struct {
    local_id: u32,
    source_local_id: u32,
    by_ref: bool = false,
    ownership: OwnershipMode = .borrow_read,
    name: []const u8,
    ty: ResolvedType,
    span: source_pkg.Span,
};

pub const CallValueExpr = struct {
    callee: *Expr,
    args: []*Expr,
    param_types: []const ResolvedType,
    // Per-parameter ownership of the callable's signature, so the backend can drop-escape
    // arguments consumed by an owned/move parameter (mirrors a direct Call). Empty means
    // "treat all as owned" for backward compatibility.
    param_ownership: []const OwnershipMode = &.{},
    ty: ResolvedType,
    span: source_pkg.Span,
};

pub const ArrayExpr = struct {
    elements: []*Expr,
    ty: ResolvedType,
    span: source_pkg.Span,
};

pub const BuilderArrayExpr = struct {
    builder: BuilderBlock,
    ty: ResolvedType,
    span: source_pkg.Span,
};

pub const IndexExpr = struct {
    object: *Expr,
    index: *Expr,
    ty: ResolvedType,
    span: source_pkg.Span,
};

pub const BinaryOp = enum {
    add,
    subtract,
    multiply,
    divide,
    modulo,
    equal,
    not_equal,
    less,
    less_equal,
    greater,
    greater_equal,
    logical_and,
    logical_or,
};

pub const UnaryOp = enum {
    negate,
    not,
};

pub const FieldStorage = enum {
    immutable,
    mutable,
};

pub fn exprType(expr: Expr) ResolvedType {
    return switch (expr) {
        .integer => .{ .kind = .integer },
        .float => .{ .kind = .float },
        .string => .{ .kind = .string },
        .boolean => .{ .kind = .boolean },
        .null_ptr => |node| node.ty,
        .function_ref => |node| node.ty,
        .callback => |node| node.ty,
        .local => |node| node.ty,
        .namespace_ref => |node| node.ty,
        .parent_view => |node| node.ty,
        .c_string_to_string => |node| node.ty,
        .array_len => |node| node.ty,
        .string_len => |node| node.ty,
        .field => |node| node.ty,
        .native_state => |node| node.ty,
        .native_user_data => |node| node.ty,
        .native_recover => |node| node.ty,
        .binary => |node| node.ty,
        .unary => |node| node.ty,
        .cast => |node| node.ty,
        .conditional => |node| node.ty,
        .construct => |node| node.ty,
        .construct_enum_variant => |node| node.ty,
        .call => |node| node.ty,
        .virtual_call => |node| node.ty,
        .call_value => |node| node.ty,
        .array => |node| node.ty,
        .builder_array => |node| node.ty,
        .index => |node| node.ty,
    };
}
