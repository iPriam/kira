const std = @import("std");
const Span = @import("kira_source").Span;

pub const Program = struct {
    imports: []ImportDecl,
    decls: []Decl,
    functions: []FunctionDecl,
};

pub const Decl = union(enum) {
    annotation_decl: AnnotationDecl,
    capability_decl: CapabilityDecl,
    enum_decl: EnumDecl,
    function_decl: FunctionDecl,
    type_decl: TypeDecl,
    construct_decl: ConstructDecl,
    construct_form_decl: ConstructFormDecl,
};

pub const ImportDecl = struct {
    module_name: QualifiedName,
    alias: ?[]const u8,
    span: Span,
};

pub const NameSegment = struct {
    text: []const u8,
    span: Span,
};

pub const QualifiedName = struct {
    segments: []NameSegment,
    span: Span,
};

pub const Annotation = struct {
    name: QualifiedName,
    args: []AnnotationArg,
    block: ?AnnotationBlock,
    span: Span,
};

pub const AnnotationArg = struct {
    label: ?[]const u8,
    value: *Expr,
    span: Span,
};

pub const AnnotationBlock = struct {
    entries: []AnnotationBlockEntry,
    span: Span,
};

pub const AnnotationBlockEntry = union(enum) {
    value: AnnotationBlockValue,
    field: AnnotationBlockField,
};

pub const AnnotationBlockValue = struct {
    value: *Expr,
    span: Span,
};

pub const AnnotationBlockField = struct {
    name: []const u8,
    value: *Expr,
    span: Span,
};

pub const AnnotationDecl = struct {
    name: []const u8,
    targets: []AnnotationTarget,
    uses: []QualifiedName,
    parameters: []AnnotationParameterDecl,
    generated_members: []GeneratedMember,
    span: Span,
};

pub const CapabilityDecl = struct {
    name: []const u8,
    generated_members: []GeneratedMember,
    span: Span,
};

pub const AnnotationTarget = enum {
    class,
    struct_decl,
    function,
    construct,
    field,
};

pub const AnnotationParameterDecl = struct {
    name: []const u8,
    type_expr: *TypeExpr,
    default_value: ?*Expr,
    span: Span,
};

pub const GeneratedMember = struct {
    overridable: bool,
    member: BodyMember,
    span: Span,
};

pub const FunctionDecl = struct {
    annotations: []const Annotation,
    is_override: bool = false,
    name: []const u8,
    params: []ParamDecl,
    return_type: ?*TypeExpr,
    body: ?Block,
    span: Span,
};

pub const FunctionSignature = struct {
    name: []const u8,
    params: []ParamDecl,
    return_type: ?*TypeExpr,
    span: Span,
};

pub const ParamDecl = struct {
    annotations: []const Annotation,
    name: []const u8,
    type_expr: ?*TypeExpr,
    span: Span,
};

pub const EnumDecl = struct {
    name: []const u8,
    type_params: [][]const u8,
    variants: []EnumVariantDecl,
    span: Span,
};

pub const EnumVariantDecl = struct {
    name: []const u8,
    associated_type: ?*TypeExpr,
    default_value: ?*Expr,
    span: Span,
};

pub const TypeDecl = struct {
    kind: TypeKind,
    annotations: []const Annotation,
    name: []const u8,
    parents: []QualifiedName,
    members: []BodyMember,
    span: Span,
};

pub const TypeKind = enum {
    class,
    struct_decl,
};

pub const ConstructDecl = struct {
    annotations: []const Annotation,
    name: []const u8,
    sections: []ConstructSection,
    span: Span,
};

pub const ConstructSection = struct {
    name: []const u8,
    kind: ConstructSectionKind,
    entries: []ConstructSectionEntry,
    span: Span,
};

pub const ConstructSectionKind = enum {
    annotations,
    modifiers,
    requires,
    lifecycle,
    builder,
    representation,
    custom,
};

pub const ConstructSectionEntry = union(enum) {
    annotation_spec: AnnotationSpec,
    field_decl: FieldDecl,
    lifecycle_hook: LifecycleHook,
    function_signature: FunctionSignature,
    named_rule: NamedRule,
};

pub const AnnotationSpec = struct {
    name: QualifiedName,
    type_expr: ?*TypeExpr,
    default_value: ?*Expr,
    span: Span,
};

pub const NamedRule = struct {
    name: QualifiedName,
    args: []RuleArg,
    type_expr: ?*TypeExpr,
    value: ?*Expr,
    block: ?Block,
    span: Span,
};

pub const RuleArg = struct {
    label: ?[]const u8,
    value: ?*Expr,
    span: Span,
};

pub const ConstructFormDecl = struct {
    annotations: []const Annotation,
    construct_name: QualifiedName,
    name: []const u8,
    params: []ParamDecl,
    body: ConstructBody,
    span: Span,
};

pub const ConstructBody = struct {
    members: []BodyMember,
    span: Span,
};

pub const BodyMember = union(enum) {
    field_decl: FieldDecl,
    function_decl: FunctionDecl,
    content_section: ContentSection,
    lifecycle_hook: LifecycleHook,
    named_rule: NamedRule,
};

pub const FieldDecl = struct {
    annotations: []const Annotation,
    is_override: bool = false,
    storage: FieldStorage,
    name: []const u8,
    type_expr: ?*TypeExpr,
    value: ?*Expr,
    span: Span,
};

pub const FieldStorage = enum {
    immutable,
    mutable,
};

pub const ContentSection = struct {
    annotations: []const Annotation,
    builder: BuilderBlock,
    span: Span,
};

pub const LifecycleHook = struct {
    name: []const u8,
    args: []RuleArg,
    body: Block,
    span: Span,
};

pub const Block = struct {
    statements: []Statement,
    span: Span,
};

pub const Statement = union(enum) {
    let_stmt: LetStatement,
    assign_stmt: AssignStatement,
    expr_stmt: ExprStatement,
    return_stmt: ReturnStatement,
    if_stmt: IfStatement,
    for_stmt: ForStatement,
    while_stmt: WhileStatement,
    break_stmt: BreakStatement,
    continue_stmt: ContinueStatement,
    match_stmt: MatchStatement,
    switch_stmt: SwitchStatement,
};

pub const LetStatement = struct {
    annotations: []const Annotation,
    storage: FieldStorage,
    name: []const u8,
    type_expr: ?*TypeExpr,
    value: ?*Expr,
    span: Span,
};

pub const ExprStatement = struct {
    expr: *Expr,
    span: Span,
};

pub const AssignStatement = struct {
    target: *Expr,
    value: *Expr,
    span: Span,
};

pub const ReturnStatement = struct {
    value: ?*Expr,
    span: Span,
};

pub const IfStatement = struct {
    condition: *Expr,
    then_block: Block,
    else_block: ?Block,
    span: Span,
};

pub const ForStatement = struct {
    binding_name: []const u8,
    iterator: *Expr,
    body: Block,
    span: Span,
};

pub const WhileStatement = struct {
    condition: *Expr,
    body: Block,
    span: Span,
};

pub const BreakStatement = struct {
    span: Span,
};

pub const ContinueStatement = struct {
    span: Span,
};

pub const MatchStatement = struct {
    subject: *Expr,
    arms: []MatchArm,
    span: Span,
};

pub const MatchArm = struct {
    patterns: []MatchPattern,
    guard: ?*Expr,
    body: Block,
    span: Span,
};

pub const MatchPattern = union(enum) {
    bare_variant: struct { name: []const u8, span: Span },
    destructure: struct { variant_name: []const u8, inner: *MatchPattern, span: Span },
    as_binding: struct { inner: *MatchPattern, binding_name: []const u8, span: Span },
};

pub const SwitchStatement = struct {
    subject: *Expr,
    cases: []SwitchCase,
    default_block: ?Block,
    span: Span,
};

pub const SwitchCase = struct {
    pattern: *Expr,
    body: Block,
    span: Span,
};

pub const BuilderBlock = struct {
    items: []BuilderItem,
    span: Span,
};

pub const BuilderItem = union(enum) {
    expr: BuilderExprItem,
    if_item: BuilderIfItem,
    for_item: BuilderForItem,
    switch_item: BuilderSwitchItem,
};

pub const BuilderExprItem = struct {
    expr: *Expr,
    span: Span,
};

pub const BuilderIfItem = struct {
    condition: *Expr,
    then_block: BuilderBlock,
    else_block: ?BuilderBlock,
    span: Span,
};

pub const BuilderForItem = struct {
    binding_name: []const u8,
    iterator: *Expr,
    body: BuilderBlock,
    span: Span,
};

pub const BuilderSwitchItem = struct {
    subject: *Expr,
    cases: []BuilderSwitchCase,
    default_block: ?BuilderBlock,
    span: Span,
};

pub const BuilderSwitchCase = struct {
    pattern: *Expr,
    body: BuilderBlock,
    span: Span,
};

pub const Expr = union(enum) {
    integer: IntegerLiteral,
    float: FloatLiteral,
    string: StringLiteral,
    bool: BoolLiteral,
    identifier: IdentifierExpr,
    array: ArrayExpr,
    callback: CallbackBlock,
    struct_literal: StructLiteralExpr,
    native_state: NativeStateExpr,
    native_user_data: NativeUserDataExpr,
    native_recover: NativeRecoverExpr,
    ownership: OwnershipExpr,
    unary: UnaryExpr,
    binary: BinaryExpr,
    conditional: ConditionalExpr,
    member: MemberExpr,
    index: IndexExpr,
    call: CallExpr,
};

pub const IntegerLiteral = struct {
    value: i64,
    span: Span,
};

pub const FloatLiteral = struct {
    value: f64,
    span: Span,
};

pub const StringLiteral = struct {
    value: []const u8,
    span: Span,
};

pub const BoolLiteral = struct {
    value: bool,
    span: Span,
};

pub const IdentifierExpr = struct {
    name: QualifiedName,
    span: Span,
};

pub const ArrayExpr = struct {
    elements: []*Expr,
    span: Span,
};

pub const StructLiteralExpr = struct {
    type_name: QualifiedName,
    fields: []StructLiteralField,
    span: Span,
};

pub const StructLiteralField = struct {
    name: []const u8,
    value: *Expr,
    span: Span,
};

pub const NativeStateExpr = struct {
    value: *Expr,
    span: Span,
};

pub const NativeUserDataExpr = struct {
    state: *Expr,
    span: Span,
};

pub const NativeRecoverExpr = struct {
    state_type: *TypeExpr,
    value: *Expr,
    span: Span,
};

pub const OwnershipExpr = struct {
    op: OwnershipExprOp,
    operand: *Expr,
    span: Span,
};

pub const OwnershipExprOp = enum {
    move,
    copy,
};

pub const UnaryExpr = struct {
    op: UnaryOp,
    operand: *Expr,
    span: Span,
};

pub const BinaryExpr = struct {
    op: BinaryOp,
    lhs: *Expr,
    rhs: *Expr,
    span: Span,
};

pub const ConditionalExpr = struct {
    condition: *Expr,
    then_expr: *Expr,
    else_expr: *Expr,
    span: Span,
};

pub const MemberExpr = struct {
    object: *Expr,
    member: []const u8,
    span: Span,
};

pub const IndexExpr = struct {
    object: *Expr,
    index: *Expr,
    span: Span,
};

pub const CallExpr = struct {
    callee: *Expr,
    args: []CallArg,
    trailing_builder: ?BuilderBlock,
    trailing_callback: ?CallbackBlock,
    span: Span,
};

pub const CallArg = struct {
    label: ?[]const u8,
    value: *Expr,
    span: Span,
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

pub const TypeExpr = union(enum) {
    named: QualifiedName,
    generic: GenericTypeExpr,
    ownership: OwnershipTypeExpr,
    any: AnyTypeExpr,
    array: ArrayTypeExpr,
    function: FunctionTypeExpr,
};

pub const OwnershipMode = enum {
    owned,
    borrow_read,
    borrow_mut,
    move,
    copy,
};

pub const OwnershipTypeExpr = struct {
    mode: OwnershipMode,
    target: *TypeExpr,
    span: Span,
};

pub const AnyTypeExpr = struct {
    target: *TypeExpr,
    span: Span,
};

pub const ArrayTypeExpr = struct {
    element_type: *TypeExpr,
    span: Span,
};

pub const FunctionTypeExpr = struct {
    params: []*TypeExpr,
    result: *TypeExpr,
    span: Span,
};

pub const GenericTypeExpr = struct {
    base: QualifiedName,
    args: []*TypeExpr,
    span: Span,
};

pub const CallbackBlock = struct {
    params: []CallbackParam,
    body: Block,
    span: Span,
};

pub const CallbackParam = struct {
    name: []const u8,
    span: Span,
};

pub const dumpProgram = @import("ast_dump.zig").dumpProgram;
