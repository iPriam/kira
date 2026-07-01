const std = @import("std");
const Span = @import("kira_source").Span;

pub const Program = struct {
    imports: []ImportDecl,
    decls: []Decl,
    functions: []FunctionDecl,
    import_origins: []DeclOrigin = &.{},
    decl_origins: []DeclOrigin = &.{},
    function_origins: []DeclOrigin = &.{},
};

pub const DeclOrigin = struct {
    package_name: ?[]const u8 = null,
    source_path: []const u8 = "",
};

pub const Decl = union(enum) {
    annotation_decl: AnnotationDecl,
    capability_decl: CapabilityDecl,
    enum_decl: EnumDecl,
    function_decl: FunctionDecl,
    type_decl: TypeDecl,
    construct_decl: ConstructDecl,
    construct_form_decl: ConstructFormDecl,
    extend_decl: ExtendDecl,
    macro_decl: MacroDecl,
    // A top-level `name!(args)` invocation of a function-position procedural macro. The
    // macro-expansion pass replaces it with the declarations the macro generates.
    macro_invocation: CallExpr,
};

// `extend Widget { function padding(...) -> Widget { ... } }` adds fluent modifier functions to a
// construct family. The target is a construct name; members are functions. This is the external,
// fluent-modifier surface (e.g. `.padding(...)`), distinct from the core Widget->Node bridge.
pub const ExtendDecl = struct {
    annotations: []const Annotation,
    construct_name: QualifiedName,
    members: []BodyMember,
    span: Span,
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
    is_comptime: bool = false,
    name: []const u8,
    params: []ParamDecl,
    return_type: ?*TypeExpr,
    body: ?Block,
    span: Span,
};

pub const FunctionSignature = struct {
    annotations: []const Annotation = &.{},
    name: []const u8,
    params: []ParamDecl,
    return_type: ?*TypeExpr,
    span: Span,
};

// --- Macros -----------------------------------------------------------------
//
// `macro Name(p: expr) { expand { ... } }` is a declarative macro: a fixed template with
// fragment substitution, hygiene, and single-evaluation `expr` semantics. `comptime macro
// Name { kind { function|attribute|derive } appliesTo { ... } expand(...) -> Syntax { ... } }`
// is a procedural macro whose `expand` runs at compile time. Both are consumed and removed by
// the macro-expansion pass (see kira_build/src/macro_expand.zig) before semantic analysis, so
// `macro_decl` and macro-call nodes never reach HIR lowering.

pub const MacroKind = enum {
    declarative,
    proc_function,
    proc_attribute,
    proc_derive,
};

// Fragment kind for a declarative macro parameter.
//   expr  - a single expression, captured call-by-value (evaluated exactly once).
//   place - an assignable lvalue path, substituted by reference (for swap-style macros).
pub const FragmentKind = enum {
    expr,
    place,
};

pub const MacroParam = struct {
    name: []const u8,
    kind: FragmentKind,
    span: Span,
};

// Declaration kinds an attribute/derive macro may legally apply to.
pub const MacroTargetKind = enum {
    struct_target,
    class_target,
    enum_target,
};

pub const MacroDecl = struct {
    annotations: []const Annotation = &.{},
    kind: MacroKind,
    name: []const u8,
    // Declarative macros: fragment parameters + the `expand { ... }` template block.
    params: []MacroParam = &.{},
    expand_block: ?Block = null,
    // Procedural macros: legal targets (attribute/derive only) + the `expand(...) -> Syntax`
    // function carrying the compile-time body.
    applies_to: []MacroTargetKind = &.{},
    expand_fn: ?FunctionDecl = null,
    span: Span,
};

pub const ParamDecl = struct {
    annotations: []const Annotation,
    name: []const u8,
    type_expr: ?*TypeExpr,
    default_value: ?*Expr = null,
    span: Span,
};

pub const EnumDecl = struct {
    annotations: []const Annotation = &.{},
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
    is_comptime: bool = false,
    name: []const u8,
    parents: []QualifiedName,
    sections: []ConstructSection,
    // Direct, SwiftUI-style members declared at the construct body's top level, e.g.
    // `@Required let body: Widget`, `@Required function measure(...) -> Size`, or a default
    // computed member `let node: Node { body.node }`. These coexist with the older
    // section-based surface; an empty slice means the construct uses only sections.
    members: []BodyMember = &.{},
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
    properties,
    custom,
};

pub const ConstructSectionEntry = union(enum) {
    annotation_spec: AnnotationSpec,
    field_decl: FieldDecl,
    lifecycle_hook: LifecycleHook,
    function_signature: FunctionSignature,
    property_schema: PropertySchemaField,
    content_channel: ContentChannelSchema,
    content_directive: ContentDirective,
    content_projection: ContentProjection,
    named_rule: NamedRule,
};

// A content-composition directive on a construct's `content` section: `content sealed`,
// `content refine { ... }`, `content passthrough`, or `content project { ... }`.
pub const ContentDirective = struct {
    mode: ContentDirectiveMode,
    span: Span,
};

pub const ContentDirectiveMode = enum { sealed, refine, passthrough, project };

// A `content project { local as Parent.channel }` mapping: the declaration section named
// `local` fills `Parent`'s `channel`.
pub const ContentProjection = struct {
    local: []const u8,
    target_construct: QualifiedName,
    target_channel: []const u8,
    span: Span,
};

// A named content channel declared in a construct's `content { ... }` block, e.g.
// `head { accepts WebElement; count 0..1 }`. `accepts` constrains the element type a
// construct-backed declaration may place in the channel; `count` constrains how many.
pub const ContentChannelSchema = struct {
    name: []const u8,
    accepts: ?QualifiedName,
    count: ?CountRange,
    span: Span,
};

// An inclusive lower bound and optional upper bound, written `min..max` or `min..`.
// `0..` is {0, null}, `0..1` is {0, 1}, `1..` is {1, null}.
pub const CountRange = struct {
    min: u32,
    max: ?u32,
    span: Span,
};

// A typed slot in a construct's `properties { ... }` schema, e.g. `required path: String`
// or `uuid: UUID`. `required` declarations must be provided by every construct-backed
// declaration; non-required declarations are optional/defaultable.
pub const PropertySchemaField = struct {
    required: bool,
    name: []const u8,
    type_expr: ?*TypeExpr,
    default_value: ?*Expr,
    span: Span,
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
    properties_section: DeclPropertiesSection,
    lifecycle_hook: LifecycleHook,
    named_rule: NamedRule,
};

// A construct-backed declaration supplies its construct's declared properties through a
// section: `Route Home { properties { path: "/" } }`. Each entry binds a schema property
// name to a value expression. There is no constructor-style `Route Home(path: "/")` form.
pub const DeclPropertiesSection = struct {
    entries: []DeclPropertyEntry,
    span: Span,
};

pub const DeclPropertyEntry = struct {
    name: []const u8,
    value: *Expr,
    span: Span,
};

pub const FieldDecl = struct {
    annotations: []const Annotation,
    is_override: bool = false,
    storage: FieldStorage,
    name: []const u8,
    type_expr: ?*TypeExpr,
    value: ?*Expr,
    // A block-bodied computed member, e.g. `let node: Node { body.node }`. When present the
    // field is a computed default/override rather than stored state, and `value` is null.
    body: ?Block = null,
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
    attempt_stmt: AttemptStatement,
};

// Linear error handling over `Result<Value, Failure>`. Each `try` expression inside `body`
// unwraps a `Result`: on `Ok` execution continues, on `Error` control transfers to the matching
// `handle` case. `handle` is a contextual keyword recognized only after an `attempt { ... }` block.
pub const AttemptStatement = struct {
    body: []Statement,
    handlers: []HandleCase,
    span: Span,
};

pub const HandleCase = struct {
    variant_name: []const u8,
    binding_name: ?[]const u8,
    body: Block,
    span: Span,
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
    // `iterator` is the iterable for `for x in collection`, or the START bound for
    // a numeric range `for i in start..end` (when `range_end` is non-null).
    iterator: *Expr,
    range_end: ?*Expr = null,
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
    builder_array: BuilderArrayExpr,
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
    try_expr: TryExpr,
    quote: QuoteExpr,
};

// `quote { ... }` inside a `comptime macro` body. The body is captured as a sequence of literal
// token runs interleaved with `#{ expr }` splice holes; the compile-time evaluator renders it to
// Kira source text (filling splices by value type) and the macro-expansion pass re-parses the
// result. Only ever appears inside a procedural macro's `expand` and is consumed before semantics.
pub const QuoteExpr = struct {
    parts: []QuotePart,
    span: Span,
};

pub const QuotePart = union(enum) {
    /// Literal source text (a run of captured token lexemes joined by spaces).
    text: []const u8,
    /// A `#{ expr }` splice; `expr` is evaluated at macro-expansion time.
    splice: *Expr,
};

// `try expr` unwraps a `Result<Value, Failure>`. Only valid inside an `attempt { ... }` block.
pub const TryExpr = struct {
    operand: *Expr,
    span: Span,
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

pub const BuilderArrayExpr = struct {
    builder: BuilderBlock,
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
    // When true this is a macro call written `name!(args)`; `callee` names the macro. The
    // macro-expansion pass replaces every such node before semantics; a residual `is_macro`
    // call reaching HIR lowering is an internal error (unknown macro already diagnosed).
    is_macro: bool = false,
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
    // `some Target` (existential / dynamic dispatch) vs `any Target`. Both currently lower to the
    // same existential `construct_any` semantics; this flag distinguishes the surface keyword so a
    // later phase can give `any` monomorphized-generic meaning while `some` keeps dynamic dispatch.
    existential: bool = false,
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
