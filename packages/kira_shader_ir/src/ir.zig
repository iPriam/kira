const source = @import("kira_source");
const shader_model = @import("kira_shader_model");

pub const Program = struct {
    imported_modules: []const ImportedModule,
    types: []const TypeDecl,
    functions: []const FunctionDecl,
    shaders: []const ShaderDecl,
};

pub const ImportedModule = struct {
    alias: []const u8,
    module_name: []const u8,
};

pub const TypeDecl = struct {
    name: []const u8,
    fields: []const FieldDecl,
    uniform_layout: ?StructLayout = null,
    storage_layout: ?StructLayout = null,
    span: source.Span,
};

pub const FieldDecl = struct {
    name: []const u8,
    ty: shader_model.Type,
    builtin: ?shader_model.Builtin = null,
    interpolation: ?shader_model.Interpolation = null,
    span: source.Span,
};

pub const StructLayout = struct {
    alignment: u32,
    size: u32,
    fields: []const FieldLayout,
};

pub const FieldLayout = struct {
    name: []const u8,
    offset: u32,
    alignment: u32,
    size: u32,
    stride: u32 = 0,
};

pub const ShaderDecl = struct {
    name: []const u8,
    kind: shader_model.module.ShaderKind,
    options: []const OptionDecl,
    groups: []const GroupDecl,
    stages: []const StageDecl,
    reflection: shader_model.reflection.Reflection,
    span: source.Span,
};

pub const OptionDecl = struct {
    name: []const u8,
    ty: shader_model.Type,
    default_value: ConstValue,
    span: source.Span,
};

pub const GroupDecl = struct {
    name: []const u8,
    class: shader_model.module.GroupClass,
    resources: []const ResourceDecl,
    span: source.Span,
};

pub const ResourceDecl = struct {
    name: []const u8,
    kind: shader_model.module.ResourceKind,
    access: ?shader_model.AccessMode,
    ty: shader_model.Type,
    visibility: []const shader_model.Stage,
    logical_group_index: u32,
    logical_binding_index: u32,
    span: source.Span,
};

pub const Threads = struct {
    x: u32,
    y: u32,
    z: u32,
};

pub const StageDecl = struct {
    kind: shader_model.Stage,
    input_type: ?[]const u8,
    output_type: ?[]const u8,
    threads: ?Threads,
    entry: FunctionDecl,
    span: source.Span,
};

pub const FunctionDecl = struct {
    name: []const u8,
    params: []const ParamDecl,
    return_type: shader_model.Type,
    body: Block,
    module_alias: ?[]const u8 = null,
    span: source.Span,
};

pub const ParamDecl = struct {
    name: []const u8,
    ty: shader_model.Type,
    span: source.Span,
};

pub const Block = struct {
    statements: []const Statement,
    span: source.Span,
};

pub const Statement = union(enum) {
    let_stmt: LetStatement,
    assign_stmt: AssignStatement,
    expr_stmt: ExprStatement,
    return_stmt: ReturnStatement,
    if_stmt: IfStatement,
};

pub const LetStatement = struct {
    name: []const u8,
    ty: shader_model.Type,
    value: ?*Expr,
    span: source.Span,
};

pub const AssignStatement = struct {
    target: *Expr,
    value: *Expr,
    span: source.Span,
};

pub const ExprStatement = struct {
    expr: *Expr,
    span: source.Span,
};

pub const ReturnStatement = struct {
    value: ?*Expr,
    span: source.Span,
};

pub const IfStatement = struct {
    condition: *Expr,
    then_block: Block,
    else_block: ?Block,
    span: source.Span,
};

pub const Expr = struct {
    ty: shader_model.Type,
    span: source.Span,
    node: Node,

    pub const Node = union(enum) {
        const_value: ConstValue,
        name: NameRef,
        unary: UnaryExpr,
        binary: BinaryExpr,
        call: CallExpr,
        member: MemberExpr,
        index: IndexExpr,
    };
};

pub const NameKind = enum {
    local,
    param,
    option,
    resource,
    function,
    imported_function,
};

pub const NameRef = struct {
    kind: NameKind,
    name: []const u8,
    module_alias: ?[]const u8 = null,
};

pub const UnaryExpr = struct {
    op: UnaryOp,
    operand: *Expr,
};

pub const UnaryOp = enum {
    neg,
    not,
};

pub const BinaryExpr = struct {
    op: BinaryOp,
    left: *Expr,
    right: *Expr,
};

pub const BinaryOp = enum {
    add,
    sub,
    mul,
    div,
    less,
    less_equal,
    greater,
    greater_equal,
    equal,
    not_equal,
};

pub const CallExpr = struct {
    callee: Callee,
    args: []const *Expr,
};

pub const Callee = union(enum) {
    function: NameRef,
    constructor: shader_model.Type,
    intrinsic: Intrinsic,
};

pub const Intrinsic = enum {
    mul,
    normalize,
    dot,
    sample,
    length,
    pow,
    sin,
    atan2,
    smoothstep,
};

pub const MemberExpr = struct {
    object: *Expr,
    name: []const u8,
};

pub const IndexExpr = struct {
    object: *Expr,
    index: *Expr,
};

pub const ConstValue = union(enum) {
    bool: bool,
    int: i32,
    uint: u32,
    float: f32,
};
