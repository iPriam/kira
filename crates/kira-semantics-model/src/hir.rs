//! HIR items and statements: programs, constructs, type declarations, and
//! function bodies.
//!
//! Ported from kira-zig `kira_semantics_model/src/hir.zig` (item/statement
//! half; expressions live in [`crate::hir_expr`]). Zig `*Expr` pointers become
//! [`ExprId`] arena indices.

use kira_runtime_abi::FunctionExecution;

use crate::ffi;
use crate::hir_expr::{BuilderBlock, ExprArena, ExprId};
use crate::span::Span;
use crate::symbols::LocalSymbol;
use crate::types::{ConstructConstraint, OwnershipMode, ResolvedType};

/// A fully analyzed program (Zig `Program`).
#[derive(Debug, Clone, Default)]
pub struct Program {
    /// Zig `imports: []Import`.
    pub imports: Vec<Import>,
    /// Zig `annotations: []AnnotationDecl`.
    pub annotations: Vec<AnnotationDecl>,
    /// Zig `capabilities: []CapabilityDecl`.
    pub capabilities: Vec<CapabilityDecl>,
    /// Zig `enums: []EnumDecl`.
    pub enums: Vec<EnumDecl>,
    /// Zig `constructs: []Construct`.
    pub constructs: Vec<Construct>,
    /// Zig `types: []TypeDecl`.
    pub types: Vec<TypeDecl>,
    /// Zig `forms: []ConstructForm`.
    pub forms: Vec<ConstructForm>,
    /// Zig `tests: []TestCase`.
    pub tests: Vec<TestCase>,
    /// Zig `functions: []Function`.
    pub functions: Vec<Function>,
    /// Zig `entry_index: usize`.
    pub entry_index: usize,
    /// Expression storage for the whole program (replaces the Zig HIR's
    /// allocator-owned `*Expr` pointers; every [`ExprId`] indexes here).
    pub exprs: ExprArena,
}

/// A module import (Zig `Import`).
#[derive(Debug, Clone, PartialEq)]
pub struct Import {
    /// Zig `module_name: []const u8`.
    pub module_name: String,
    /// Zig `alias: ?[]const u8`.
    pub alias: Option<String>,
    /// Zig `package_name: ?[]const u8`.
    pub package_name: Option<String>,
    /// Zig `source_path: []const u8`.
    pub source_path: String,
    /// Zig `span: Span`.
    pub span: Span,
}

/// An applied annotation (Zig `Annotation`).
#[derive(Debug, Clone)]
pub struct Annotation {
    /// Zig `name: []const u8`.
    pub name: String,
    /// Zig `is_namespaced: bool = false`.
    pub is_namespaced: bool,
    /// Zig `symbol_index: ?usize`.
    pub symbol_index: Option<usize>,
    /// Zig `arguments: []AnnotationArgument`.
    pub arguments: Vec<AnnotationArgument>,
    /// Zig `span: Span`.
    pub span: Span,
}

/// An annotation declaration (Zig `AnnotationDecl`).
#[derive(Debug, Clone)]
pub struct AnnotationDecl {
    /// Zig `name: []const u8`.
    pub name: String,
    /// Zig `targets: []AnnotationTarget`.
    pub targets: Vec<AnnotationTarget>,
    /// Zig `uses: []const []const u8`.
    pub uses: Vec<String>,
    /// Zig `generated_functions: []GeneratedFunction`.
    pub generated_functions: Vec<GeneratedFunction>,
    /// Zig `parameters: []AnnotationParameterDecl`.
    pub parameters: Vec<AnnotationParameterDecl>,
    /// Zig `module_path: []const u8`.
    pub module_path: String,
    /// Zig `span: Span`.
    pub span: Span,
}

/// A capability declaration (Zig `CapabilityDecl`).
#[derive(Debug, Clone)]
pub struct CapabilityDecl {
    /// Zig `name: []const u8`.
    pub name: String,
    /// Zig `generated_functions: []GeneratedFunction`.
    pub generated_functions: Vec<GeneratedFunction>,
    /// Zig `module_path: []const u8`.
    pub module_path: String,
    /// Zig `span: Span`.
    pub span: Span,
}

/// An enum declaration (Zig `EnumDecl`).
#[derive(Debug, Clone)]
pub struct EnumDecl {
    /// Zig `name: []const u8`.
    pub name: String,
    /// Zig `type_params: [][]const u8`.
    pub type_params: Vec<String>,
    /// Zig `variants: []EnumVariantHir`.
    pub variants: Vec<EnumVariantHir>,
    /// Zig `derive_copy: bool` — the `@Derive(Copy)` opt-in copyability
    /// assertion, verified structurally by the mid-IR checker (KIR005).
    pub derive_copy: bool,
    /// Zig `span: Span`.
    pub span: Span,
}

/// An enum variant (Zig `EnumVariantHir`).
#[derive(Debug, Clone)]
pub struct EnumVariantHir {
    /// Zig `name: []const u8`.
    pub name: String,
    /// Zig `discriminant: u32`.
    pub discriminant: u32,
    /// Zig `payload_ty: ?ResolvedType`.
    pub payload_ty: Option<ResolvedType>,
    /// Zig `default_value: ?*Expr`.
    pub default_value: Option<ExprId>,
    /// Zig `span: Span`.
    pub span: Span,
}

/// What an annotation may attach to (Zig `AnnotationTarget`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AnnotationTarget {
    /// Zig `.class`.
    Class,
    /// Zig `.struct_decl`.
    StructDecl,
    /// Zig `.function`.
    Function,
    /// Zig `.construct`.
    Construct,
    /// Zig `.field`.
    Field,
}

/// A function an annotation/capability generates (Zig `GeneratedFunction`).
#[derive(Debug, Clone)]
pub struct GeneratedFunction {
    /// Zig `name: []const u8`.
    pub name: String,
    /// Zig `overridable: bool`.
    pub overridable: bool,
    /// Zig `params: []const ResolvedType`.
    pub params: Vec<ResolvedType>,
    /// Zig `param_ownership: []const OwnershipMode`.
    pub param_ownership: Vec<OwnershipMode>,
    /// Zig `return_type: ResolvedType = .{ .kind = .unknown }`.
    pub return_type: ResolvedType,
    /// Zig `return_ownership: OwnershipMode = .owned`.
    pub return_ownership: OwnershipMode,
    /// Zig `source_annotation: []const u8`.
    pub source_annotation: String,
    /// Zig `span: Span`.
    pub span: Span,
}

/// An annotation parameter declaration (Zig `AnnotationParameterDecl`).
#[derive(Debug, Clone)]
pub struct AnnotationParameterDecl {
    /// Zig `name: []const u8`.
    pub name: String,
    /// Zig `ty: ResolvedType`.
    pub ty: ResolvedType,
    /// Zig `default_value: ?AnnotationValue`.
    pub default_value: Option<AnnotationValue>,
    /// Zig `span: Span`.
    pub span: Span,
}

/// An argument passed to an annotation (Zig `AnnotationArgument`).
#[derive(Debug, Clone)]
pub struct AnnotationArgument {
    /// Zig `name: []const u8`.
    pub name: String,
    /// Zig `value: AnnotationValue`.
    pub value: AnnotationValue,
    /// Zig `ty: ResolvedType`.
    pub ty: ResolvedType,
    /// Zig `span: Span`.
    pub span: Span,
}

/// A literal annotation value (Zig `AnnotationValue`).
#[derive(Debug, Clone, PartialEq)]
pub enum AnnotationValue {
    /// Zig `.integer: i64`.
    Integer(i64),
    /// Zig `.float: f64`.
    Float(f64),
    /// Zig `.boolean: bool`.
    Boolean(bool),
    /// Zig `.string: []const u8`.
    String(String),
}

/// A construct declaration — the SwiftUI-style protocol surface (Zig `Construct`).
#[derive(Debug, Clone, Default)]
pub struct Construct {
    /// Zig `name: []const u8`.
    pub name: String,
    /// Zig `parents: []ConstructParent`.
    pub parents: Vec<ConstructParent>,
    /// Zig `properties: []PropertySchema`.
    pub properties: Vec<PropertySchema>,
    /// Zig `content_channels: []ContentChannel`.
    pub content_channels: Vec<ContentChannel>,
    /// Zig `content_refine: []ContentChannel`.
    pub content_refine: Vec<ContentChannel>,
    /// Zig `content_projections: []ContentProjection`.
    pub content_projections: Vec<ContentProjection>,
    /// Zig `content_sealed: bool`.
    pub content_sealed: bool,
    /// Zig `content_passthrough: bool`.
    pub content_passthrough: bool,
    /// Zig `required_functions: []RequiredFunction`.
    pub required_functions: Vec<RequiredFunction>,
    /// Zig `section_functions: []SectionFunction`.
    pub section_functions: Vec<SectionFunction>,
    /// Zig `consuming_functions` — family methods declared `@Consuming` (the
    /// call consumes the receiver; virtual dispatch transfers ownership).
    pub consuming_functions: Vec<String>,
    /// Zig `required_fields` — direct `@Required let name: T` members.
    pub required_fields: Vec<RequiredField>,
    /// Zig `default_members` — non-required members with default bodies
    /// (overriding them can discharge requirements; terminal-`node` rule).
    pub default_members: Vec<ConstructDefaultMember>,
    /// Zig `allowed_annotations: []AnnotationRule`.
    pub allowed_annotations: Vec<AnnotationRule>,
    /// Zig `content_element_type` — element type of a typed
    /// `content: Content<T>;` section (e.g. "Widget"); None when unpinned.
    pub content_element_type: Option<String>,
    /// Zig `allowed_lifecycle_hooks: [][]const u8`.
    pub allowed_lifecycle_hooks: Vec<String>,
    /// Zig `span: Span`.
    pub span: Span,
}

/// A section function requirement on a construct (Zig `SectionFunction`).
#[derive(Debug, Clone)]
pub struct SectionFunction {
    /// Zig `name: []const u8`.
    pub name: String,
    /// Zig `required: bool`.
    pub required: bool,
    /// Zig `param_types: []const []const u8` (canonical type texts).
    pub param_types: Vec<String>,
    /// Zig `return_type: []const u8`.
    pub return_type: String,
    /// Zig `span: Span`.
    pub span: Span,
}

/// An allowed-annotation rule on a construct (Zig `AnnotationRule`).
#[derive(Debug, Clone)]
pub struct AnnotationRule {
    /// Zig `name: []const u8`.
    pub name: String,
    /// Zig `span: Span`.
    pub span: Span,
}

/// A parent construct reference (Zig `ConstructParent`).
#[derive(Debug, Clone)]
pub struct ConstructParent {
    /// Zig `name: []const u8`.
    pub name: String,
    /// Zig `span: Span`.
    pub span: Span,
}

/// A typed slot in a construct's `properties { ... }` schema (Zig `PropertySchema`).
#[derive(Debug, Clone)]
pub struct PropertySchema {
    /// Zig `required: bool`.
    pub required: bool,
    /// Zig `name: []const u8`.
    pub name: String,
    /// Zig `type_text: []const u8` — canonical type text used for validation.
    pub type_text: String,
    /// Zig `span: Span`.
    pub span: Span,
}

/// A function a construct requires of its first concrete declaration
/// (Zig `RequiredFunction`; `Self` stays literal in the type texts).
#[derive(Debug, Clone)]
pub struct RequiredFunction {
    /// Zig `name: []const u8`.
    pub name: String,
    /// Zig `param_types: []const []const u8`.
    pub param_types: Vec<String>,
    /// Zig `return_type: []const u8`.
    pub return_type: String,
    /// Zig `span: Span`.
    pub span: Span,
}

/// A `@Required let name: T` member on a construct (Zig `RequiredField`).
#[derive(Debug, Clone)]
pub struct RequiredField {
    /// Zig `name: []const u8`.
    pub name: String,
    /// Zig `type_text: []const u8`.
    pub type_text: String,
    /// Zig `span: Span`.
    pub span: Span,
}

/// A non-required construct member with a default body (Zig `ConstructDefaultMember`).
#[derive(Debug, Clone)]
pub struct ConstructDefaultMember {
    /// Zig `name: []const u8`.
    pub name: String,
    /// Zig `is_field: bool`.
    pub is_field: bool,
    /// Zig `references` — required fields read by the default body.
    pub references: Vec<String>,
    /// Zig `span: Span`.
    pub span: Span,
}

/// A named content channel on a construct (Zig `ContentChannel`).
#[derive(Debug, Clone)]
pub struct ContentChannel {
    /// Zig `name: []const u8`.
    pub name: String,
    /// Zig `accepts: ?[]const u8` — canonical element type text (None = unconstrained).
    pub accepts: Option<String>,
    /// Zig `min: u32`.
    pub min: u32,
    /// Zig `max: ?u32` (None = unbounded).
    pub max: Option<u32>,
    /// Zig `span: Span`.
    pub span: Span,
}

/// A `content project { local as Parent.channel }` mapping (Zig `ContentProjection`).
#[derive(Debug, Clone)]
pub struct ContentProjection {
    /// Zig `local: []const u8`.
    pub local: String,
    /// Zig `target_construct: []const u8`.
    pub target_construct: String,
    /// Zig `target_channel: []const u8`.
    pub target_channel: String,
    /// Zig `span: Span`.
    pub span: Span,
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
    /// Zig `kind: TypeKind = .struct_decl`.
    pub kind: TypeKind,
    /// Zig `name: []const u8`.
    pub name: String,
    /// Zig `execution: FunctionExecution = .inherited`.
    pub execution: FunctionExecution,
    /// Zig `fields: []const Field`.
    pub fields: Vec<Field>,
    /// Zig `methods: []const MethodMember`.
    pub methods: Vec<MethodMember>,
    /// Zig `ffi: ?NamedTypeInfo`.
    pub ffi: Option<ffi::NamedTypeInfo>,
    /// Zig `derive_copy: bool` — `@Derive(Copy)` assertion (KIR005).
    pub derive_copy: bool,
    /// Zig `span: Span`.
    pub span: Span,
}

/// A method member of a type (Zig `MethodMember`).
#[derive(Debug, Clone)]
pub struct MethodMember {
    /// Zig `name: []const u8`.
    pub name: String,
    /// Zig `full_name: []const u8`.
    pub full_name: String,
    /// Zig `receiver_offset: u32`.
    pub receiver_offset: u32,
    /// Zig `span: Span`.
    pub span: Span,
}

/// A concrete construct-backed declaration (Zig `ConstructForm`).
#[derive(Debug, Clone)]
pub struct ConstructForm {
    /// Zig `construct: ConstructConstraint`.
    pub construct: ConstructConstraint,
    /// Zig `name: []const u8`.
    pub name: String,
    /// Zig `families: []const []const u8`.
    pub families: Vec<String>,
    /// Zig `fields: []const Field`.
    pub fields: Vec<Field>,
    /// Zig `content: ?BuilderBlock`.
    pub content: Option<BuilderBlock>,
    /// Zig `lifecycle_hooks: []LifecycleHook`.
    pub lifecycle_hooks: Vec<LifecycleHook>,
    /// Zig `span: Span`.
    pub span: Span,
}

/// A `Test` construct case (Zig `TestCase`).
#[derive(Debug, Clone)]
pub struct TestCase {
    /// Zig `name: []const u8`.
    pub name: String,
    /// Zig `test_function: []const u8`.
    pub test_function: String,
    /// Zig `expect_function: []const u8`.
    pub expect_function: String,
    /// Zig `result_type: ResolvedType`.
    pub result_type: ResolvedType,
    /// Zig `span: Span`.
    pub span: Span,
}

/// A field on a type or construct form (Zig `Field`).
#[derive(Debug, Clone)]
pub struct Field {
    /// Zig `name: []const u8`.
    pub name: String,
    /// Zig `owner_type_name: []const u8`.
    pub owner_type_name: String,
    /// Zig `storage: FieldStorage`.
    pub storage: FieldStorage,
    /// Zig `slot_index: u32`.
    pub slot_index: u32,
    /// Zig `ty: ResolvedType`.
    pub ty: ResolvedType,
    /// Zig `explicit_type: bool`.
    pub explicit_type: bool,
    /// Zig `default_value: ?*Expr`.
    pub default_value: Option<ExprId>,
    /// Zig `annotations: []Annotation`.
    pub annotations: Vec<Annotation>,
    /// Zig `span: Span`.
    pub span: Span,
}

/// Mutability of a field or local slot (Zig `FieldStorage`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum FieldStorage {
    /// Zig `.immutable`.
    #[default]
    Immutable,
    /// Zig `.mutable`.
    Mutable,
}

/// A lifecycle hook on a construct form (Zig `LifecycleHook`).
#[derive(Debug, Clone)]
pub struct LifecycleHook {
    /// Zig `name: []const u8`.
    pub name: String,
    /// Zig `span: Span`.
    pub span: Span,
}

/// An analyzed function (Zig `Function`).
#[derive(Debug, Clone)]
pub struct Function {
    /// Zig `id: u32`.
    pub id: u32,
    /// Zig `name: []const u8`.
    pub name: String,
    /// Zig `is_main: bool`.
    pub is_main: bool,
    /// Zig `is_async: bool = false`.
    pub is_async: bool,
    /// Zig `execution: FunctionExecution`.
    pub execution: FunctionExecution,
    /// Zig `is_extern: bool = false`.
    pub is_extern: bool,
    /// Zig `foreign: ?ForeignFunction`.
    pub foreign: Option<ffi::ForeignFunction>,
    /// Zig `annotations: []Annotation`.
    pub annotations: Vec<Annotation>,
    /// Zig `params: []Parameter`.
    pub params: Vec<Parameter>,
    /// Zig `locals: []LocalSymbol`.
    pub locals: Vec<LocalSymbol>,
    /// Zig `return_type: ResolvedType`.
    pub return_type: ResolvedType,
    /// Zig `return_ownership: OwnershipMode = .owned`.
    pub return_ownership: OwnershipMode,
    /// Zig `body: []Statement`.
    pub body: Vec<Statement>,
    /// Zig `span: Span`.
    pub span: Span,
}

/// A function parameter (Zig `Parameter`).
#[derive(Debug, Clone)]
pub struct Parameter {
    /// Zig `id: u32`.
    pub id: u32,
    /// Zig `name: []const u8`.
    pub name: String,
    /// Zig `ty: ResolvedType`.
    pub ty: ResolvedType,
    /// Zig `ownership: OwnershipMode = .owned`.
    pub ownership: OwnershipMode,
    /// Zig `span: Span`.
    pub span: Span,
}

/// A statement in a function body (Zig `Statement`).
#[derive(Debug, Clone)]
pub enum Statement {
    /// Zig `.let_stmt`.
    Let(LetStatement),
    /// Zig `.assign_stmt`.
    Assign(AssignStatement),
    /// Zig `.expr_stmt`.
    Expr(ExprStatement),
    /// Zig `.if_stmt`.
    If(IfStatement),
    /// Zig `.for_stmt`.
    For(ForStatement),
    /// Zig `.while_stmt`.
    While(WhileStatement),
    /// Zig `.break_stmt`.
    Break(BreakStatement),
    /// Zig `.continue_stmt`.
    Continue(ContinueStatement),
    /// Zig `.match_stmt`.
    Match(MatchStatement),
    /// Zig `.switch_stmt`.
    Switch(SwitchStatement),
    /// Zig `.return_stmt`.
    Return(ReturnStatement),
}

/// `let`/`var` binding (Zig `LetStatement`).
#[derive(Debug, Clone)]
pub struct LetStatement {
    /// Zig `local_id: u32`.
    pub local_id: u32,
    /// Zig `ty: ResolvedType`.
    pub ty: ResolvedType,
    /// Zig `explicit_type: bool`.
    pub explicit_type: bool,
    /// Zig `value: ?*Expr`.
    pub value: Option<ExprId>,
    /// Zig `is_reborrow` — the initializer reads a borrow binding, so the new
    /// local aliases the same storage (Rust reborrow) instead of copying.
    pub is_reborrow: bool,
    /// Zig `span: Span`.
    pub span: Span,
}

/// Expression statement (Zig `ExprStatement`).
#[derive(Debug, Clone)]
pub struct ExprStatement {
    /// Zig `expr: *Expr`.
    pub expr: ExprId,
    /// Zig `span: Span`.
    pub span: Span,
}

/// Assignment (Zig `AssignStatement`).
#[derive(Debug, Clone)]
pub struct AssignStatement {
    /// Zig `target: *Expr`.
    pub target: ExprId,
    /// Zig `value: *Expr`.
    pub value: ExprId,
    /// Zig `span: Span`.
    pub span: Span,
}

/// `if`/`else` (Zig `IfStatement`).
#[derive(Debug, Clone)]
pub struct IfStatement {
    /// Zig `condition: *Expr`.
    pub condition: ExprId,
    /// Zig `then_body: []Statement`.
    pub then_body: Vec<Statement>,
    /// Zig `else_body: ?[]Statement`.
    pub else_body: Option<Vec<Statement>>,
    /// Zig `span: Span`.
    pub span: Span,
}

/// `for x in iterable` / `for i in start..end` (Zig `ForStatement`).
#[derive(Debug, Clone)]
pub struct ForStatement {
    /// Zig `binding_name: []const u8`.
    pub binding_name: String,
    /// Zig `binding_local_id: u32`.
    pub binding_local_id: u32,
    /// Zig `binding_ty: ResolvedType`.
    pub binding_ty: ResolvedType,
    /// Zig `iterator` — the iterable, or the START bound of a numeric range
    /// when `range_end` is set (half-open `[start, end)`).
    pub iterator: ExprId,
    /// Zig `range_end: ?*Expr`.
    pub range_end: Option<ExprId>,
    /// Zig `body: []Statement`.
    pub body: Vec<Statement>,
    /// Zig `span: Span`.
    pub span: Span,
}

/// `while` loop (Zig `WhileStatement`).
#[derive(Debug, Clone)]
pub struct WhileStatement {
    /// Zig `condition: *Expr`.
    pub condition: ExprId,
    /// Zig `body: []Statement`.
    pub body: Vec<Statement>,
    /// Zig `span: Span`.
    pub span: Span,
}

/// `break` (Zig `BreakStatement`).
#[derive(Debug, Clone)]
pub struct BreakStatement {
    /// Zig `span: Span`.
    pub span: Span,
}

/// `continue` (Zig `ContinueStatement`).
#[derive(Debug, Clone)]
pub struct ContinueStatement {
    /// Zig `span: Span`.
    pub span: Span,
}

/// Enum `match` (Zig `MatchStatement`).
#[derive(Debug, Clone)]
pub struct MatchStatement {
    /// Zig `subject: *Expr`.
    pub subject: ExprId,
    /// Zig `arms: []MatchArm`.
    pub arms: Vec<MatchArm>,
    /// Zig `enum_name: []const u8`.
    pub enum_name: String,
    /// Zig `span: Span`.
    pub span: Span,
}

/// One `match` arm (Zig `MatchArm`).
#[derive(Debug, Clone)]
pub struct MatchArm {
    /// Zig `pattern: MatchPattern`.
    pub pattern: MatchPattern,
    /// Zig `guard: ?*Expr`.
    pub guard: Option<ExprId>,
    /// Zig `body: []Statement`.
    pub body: Vec<Statement>,
    /// Zig `span: Span`.
    pub span: Span,
}

/// A `match` pattern (Zig `MatchPattern`).
#[derive(Debug, Clone)]
pub enum MatchPattern {
    /// Zig `.variant`.
    Variant(VariantMatchPattern),
    /// Zig `.binding`.
    Binding(BindingMatchPattern),
}

/// Enum-variant pattern (Zig `VariantMatchPattern`).
#[derive(Debug, Clone)]
pub struct VariantMatchPattern {
    /// Zig `variant_name: []const u8`.
    pub variant_name: String,
    /// Zig `discriminant: u32`.
    pub discriminant: u32,
    /// Zig `payload_ty: ?ResolvedType`.
    pub payload_ty: Option<ResolvedType>,
    /// Zig `inner: ?*MatchPattern` (boxed here — rare self-reference).
    pub inner: Option<Box<MatchPattern>>,
    /// Zig `as_binding_local_id: ?u32`.
    pub as_binding_local_id: Option<u32>,
    /// Zig `as_binding_ty: ?ResolvedType`.
    pub as_binding_ty: Option<ResolvedType>,
    /// Zig `span: Span`.
    pub span: Span,
}

/// Catch-all binding pattern (Zig `BindingMatchPattern`).
#[derive(Debug, Clone)]
pub struct BindingMatchPattern {
    /// Zig `local_id: u32`.
    pub local_id: u32,
    /// Zig `name: []const u8`.
    pub name: String,
    /// Zig `ty: ResolvedType`.
    pub ty: ResolvedType,
    /// Zig `span: Span`.
    pub span: Span,
}

/// Value `switch` (Zig `SwitchStatement`).
#[derive(Debug, Clone)]
pub struct SwitchStatement {
    /// Zig `subject: *Expr`.
    pub subject: ExprId,
    /// Zig `cases: []SwitchCase`.
    pub cases: Vec<SwitchCase>,
    /// Zig `default_body: ?[]Statement`.
    pub default_body: Option<Vec<Statement>>,
    /// Zig `span: Span`.
    pub span: Span,
}

/// One `switch` case (Zig `SwitchCase`).
#[derive(Debug, Clone)]
pub struct SwitchCase {
    /// Zig `pattern: *Expr`.
    pub pattern: ExprId,
    /// Zig `body: []Statement`.
    pub body: Vec<Statement>,
    /// Zig `span: Span`.
    pub span: Span,
}

/// `return` (Zig `ReturnStatement`).
#[derive(Debug, Clone)]
pub struct ReturnStatement {
    /// Zig `value: ?*Expr`.
    pub value: Option<ExprId>,
    /// Zig `span: Span`.
    pub span: Span,
}
