//! Declaration-level AST: programs, imports, functions, types, constructs, macros.
//!
//! Mirrors kira-zig `packages/kira_syntax_model/src/ast.zig`, translated to
//! the index/arena pattern: a [`Program`] owns [`AstArenas`], and every Zig
//! `*Expr` / `*TypeExpr` / `*MatchPattern` pointer becomes an arena index
//! ([`ExprId`] / [`TypeExprId`] / [`PatternId`]). Identifier text is interned
//! ([`Symbol`]); decoded literal / raw captured text is owned `String`.

use kira_core::Symbol;
use kira_source::Span;
use la_arena::Arena;

pub use crate::ast_exprs::*;

/// Arena storage for the pointer-allocated AST node kinds; replaces Zig's
/// per-parse arena allocator plus `*Expr`-style pointers.
#[derive(Debug, Default)]
pub struct AstArenas {
    /// All expressions of one parsed program.
    pub exprs: Arena<Expr>,
    /// All type expressions of one parsed program.
    pub type_exprs: Arena<TypeExpr>,
    /// All nested match patterns of one parsed program.
    pub patterns: Arena<MatchPattern>,
}

/// One parsed source file: top-level imports, declarations, and functions,
/// plus the arenas their nodes index into.
#[derive(Debug, Default)]
pub struct Program {
    /// Arena storage every node id of this program points into.
    pub arenas: AstArenas,
    /// The file's `import` declarations.
    pub imports: Vec<ImportDecl>,
    /// The file's non-function top-level declarations.
    pub decls: Vec<Decl>,
    /// The file's top-level functions.
    pub functions: Vec<FunctionDecl>,
    /// Origin of each import (parallel to `imports`); empty until merging.
    pub import_origins: Vec<DeclOrigin>,
    /// Origin of each decl (parallel to `decls`); empty until merging.
    pub decl_origins: Vec<DeclOrigin>,
    /// Origin of each function (parallel to `functions`); empty until merging.
    pub function_origins: Vec<DeclOrigin>,
}

/// Where a merged top-level declaration originally came from.
#[derive(Debug, Clone, Copy, Default)]
pub struct DeclOrigin {
    /// Manifest name of the package the declaration came from.
    pub package_name: Option<Symbol>,
    /// Path of the source file the declaration came from.
    pub source_path: Option<Symbol>,
    /// For an import origin: the manifest name of the package that OWNS the
    /// imported module, recorded when the written module root (`import UI`)
    /// differs from that package's name. The file-scope import gate keys
    /// dependency symbols by owner package name.
    pub module_owner_package: Option<Symbol>,
}

/// One top-level declaration (full port of the Zig `Decl` union).
#[derive(Debug, Clone)]
pub enum Decl {
    /// An `annotation` declaration.
    Annotation(AnnotationDecl),
    /// A `capability` declaration.
    Capability(CapabilityDecl),
    /// An `enum` declaration.
    Enum(EnumDecl),
    /// A `type Alias = Target` declaration.
    TypeAlias(TypeAliasDecl),
    /// A top-level function (also reachable via `Program::functions`).
    Function(FunctionDecl),
    /// A `class` / `struct` declaration.
    Type(TypeDecl),
    /// A `construct` declaration.
    Construct(ConstructDecl),
    /// A construct-backed declaration form (`Widget Counter(...) { ... }`).
    ConstructForm(ConstructFormDecl),
    /// A `FailTest` expected-compile-outcome test.
    FailTest(FailTestDecl),
    /// An `extend Construct { ... }` fluent-modifier extension.
    Extend(ExtendDecl),
    /// A `macro` declaration (declarative or procedural).
    Macro(MacroDecl),
    /// A top-level `name!(args)` procedural-macro invocation; the
    /// macro-expansion pass replaces it with generated declarations.
    MacroInvocation(CallExpr),
}

/// `extend Widget { function padding(...) -> Widget { ... } }`: adds fluent
/// modifier functions (`.padding(...)`) to a construct family.
#[derive(Debug, Clone)]
pub struct ExtendDecl {
    /// Annotations preceding the declaration.
    pub annotations: Vec<Annotation>,
    /// The extended construct's name.
    pub construct_name: QualifiedName,
    /// The added members (functions).
    pub members: Vec<BodyMember>,
    /// Source range of the declaration.
    pub span: Span,
}

/// An `import Module as Alias` declaration.
#[derive(Debug, Clone)]
pub struct ImportDecl {
    /// The imported module's dotted name.
    pub module_name: QualifiedName,
    /// The local alias, when written.
    pub alias: Option<Symbol>,
    /// Source range of the declaration.
    pub span: Span,
}

/// One segment of a dotted name, with its own span.
#[derive(Debug, Clone, Copy)]
pub struct NameSegment {
    /// The segment text.
    pub text: Symbol,
    /// Source range of the segment.
    pub span: Span,
}

/// A dotted name path like `UI.Widgets.Button`.
#[derive(Debug, Clone, Default)]
pub struct QualifiedName {
    /// The segments in source order.
    pub segments: Vec<NameSegment>,
    /// Source range of the whole name.
    pub span: Span,
}

/// One applied annotation, e.g. `@State` or `@Font(size = 12) { ... }`.
#[derive(Debug, Clone)]
pub struct Annotation {
    /// The annotation's name.
    pub name: QualifiedName,
    /// The annotation arguments in source order.
    pub args: Vec<AnnotationArg>,
    /// A trailing configuration block, when written.
    pub block: Option<AnnotationBlock>,
    /// Source range of the annotation.
    pub span: Span,
}

/// One (optionally labeled) annotation argument.
#[derive(Debug, Clone)]
pub struct AnnotationArg {
    /// The argument label, when written.
    pub label: Option<Symbol>,
    /// The argument value.
    pub value: ExprId,
    /// Source range of the argument.
    pub span: Span,
}

/// The `{ ... }` block trailing an annotation.
#[derive(Debug, Clone)]
pub struct AnnotationBlock {
    /// The block entries in source order.
    pub entries: Vec<AnnotationBlockEntry>,
    /// Source range of the block.
    pub span: Span,
}

/// One entry of an annotation block.
#[derive(Debug, Clone)]
pub enum AnnotationBlockEntry {
    /// A bare value entry.
    Value(AnnotationBlockValue),
    /// A `name = value` field entry.
    Field(AnnotationBlockField),
}

/// A bare value inside an annotation block.
#[derive(Debug, Clone)]
pub struct AnnotationBlockValue {
    /// The value expression.
    pub value: ExprId,
    /// Source range of the entry.
    pub span: Span,
}

/// A `name = value` field inside an annotation block.
#[derive(Debug, Clone)]
pub struct AnnotationBlockField {
    /// The field name.
    pub name: Symbol,
    /// The field value.
    pub value: ExprId,
    /// Source range of the entry.
    pub span: Span,
}

/// An `annotation Name targets ... uses ... { ... }` declaration.
#[derive(Debug, Clone)]
pub struct AnnotationDecl {
    /// The annotation's name.
    pub name: Symbol,
    /// The declaration kinds this annotation may apply to.
    pub targets: Vec<AnnotationTarget>,
    /// Capabilities the annotation uses.
    pub uses: Vec<QualifiedName>,
    /// The annotation's parameters.
    pub parameters: Vec<AnnotationParameterDecl>,
    /// Members generated onto annotated declarations.
    pub generated_members: Vec<GeneratedMember>,
    /// Source range of the declaration.
    pub span: Span,
}

/// A `capability Name { ... }` declaration.
#[derive(Debug, Clone)]
pub struct CapabilityDecl {
    /// The capability's name.
    pub name: Symbol,
    /// Members generated onto declarations using the capability.
    pub generated_members: Vec<GeneratedMember>,
    /// Source range of the declaration.
    pub span: Span,
}

/// Declaration kinds an annotation may target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AnnotationTarget {
    /// `class` declarations.
    Class,
    /// `struct` declarations.
    Struct,
    /// Functions.
    Function,
    /// Constructs.
    Construct,
    /// Fields.
    Field,
}

/// One parameter of an annotation declaration.
#[derive(Debug, Clone)]
pub struct AnnotationParameterDecl {
    /// The parameter name.
    pub name: Symbol,
    /// The parameter type.
    pub type_expr: TypeExprId,
    /// The default value, when written.
    pub default_value: Option<ExprId>,
    /// Source range of the parameter.
    pub span: Span,
}

/// One `generated` member of an annotation/capability declaration.
#[derive(Debug, Clone)]
pub struct GeneratedMember {
    /// True when annotated declarations may override the member.
    pub overridable: bool,
    /// The generated member itself.
    pub member: BodyMember,
    /// Source range of the member.
    pub span: Span,
}

/// A `function name(params) -> Return { ... }` declaration.
#[derive(Debug, Clone)]
pub struct FunctionDecl {
    /// Annotations preceding the function.
    pub annotations: Vec<Annotation>,
    /// True for `override function`.
    pub is_override: bool,
    /// True for `comptime function`.
    pub is_comptime: bool,
    /// True for `async function`.
    pub is_async: bool,
    /// The function name.
    pub name: Symbol,
    /// The parameters in source order.
    pub params: Vec<ParamDecl>,
    /// The declared return type, when written.
    pub return_type: Option<TypeExprId>,
    /// The body; `None` for signature-only declarations.
    pub body: Option<Block>,
    /// Source range of the declaration.
    pub span: Span,
}

/// A bodyless function signature (used in construct `requires` sections).
#[derive(Debug, Clone)]
pub struct FunctionSignature {
    /// Annotations preceding the signature.
    pub annotations: Vec<Annotation>,
    /// The function name.
    pub name: Symbol,
    /// The parameters in source order.
    pub params: Vec<ParamDecl>,
    /// The declared return type, when written.
    pub return_type: Option<TypeExprId>,
    /// Source range of the signature.
    pub span: Span,
}

/// A `type Alias = Target` declaration.
#[derive(Debug, Clone)]
pub struct TypeAliasDecl {
    /// The alias name.
    pub name: Symbol,
    /// The aliased type.
    pub target: TypeExprId,
    /// Source range of the declaration.
    pub span: Span,
}

/// How a macro expands: declarative template or procedural `expand` function.
///
/// `macro Name(p: expr) { expand { ... } }` is declarative (fixed template,
/// hygiene, single-evaluation `expr` fragments); `comptime macro Name { ... }`
/// is procedural, its `expand` running at compile time. Both are consumed by
/// the macro-expansion pass before semantic analysis.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MacroKind {
    /// A declarative template macro.
    Declarative,
    /// A procedural function-position macro (`name!(args)`).
    ProcFunction,
    /// A procedural attribute macro.
    ProcAttribute,
    /// A procedural derive macro.
    ProcDerive,
    /// The property-wrapper protocol macro (`kind { wrapper }`): validates a
    /// wrapper template struct and later expands over declarations whose
    /// fields carry the wrapper annotation, with `(target, wrapper)` bound.
    ProcWrapper,
}

/// Fragment kind for a declarative macro parameter: `expr` captures by value
/// (evaluated once), `place` substitutes an assignable path by reference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FragmentKind {
    /// A single expression, captured call-by-value.
    Expr,
    /// An assignable lvalue path, substituted by reference.
    Place,
}

/// One fragment parameter of a declarative macro.
#[derive(Debug, Clone, Copy)]
pub struct MacroParam {
    /// The parameter name.
    pub name: Symbol,
    /// The fragment kind.
    pub kind: FragmentKind,
    /// Source range of the parameter.
    pub span: Span,
}

/// Declaration kinds an attribute/derive macro may legally apply to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MacroTargetKind {
    /// `struct` declarations.
    Struct,
    /// `class` declarations.
    Class,
    /// `enum` declarations.
    Enum,
    /// Construct-backed declaration forms (`Widget Counter(...) { ... }`).
    Form,
}

/// A `macro` declaration, declarative or procedural.
#[derive(Debug, Clone)]
pub struct MacroDecl {
    /// Annotations preceding the macro.
    pub annotations: Vec<Annotation>,
    /// Declarative vs procedural kind.
    pub kind: MacroKind,
    /// The macro name.
    pub name: Symbol,
    /// Declarative macros: the fragment parameters.
    pub params: Vec<MacroParam>,
    /// Declarative macros: the `expand { ... }` template block.
    pub expand_block: Option<Block>,
    /// Procedural macros: legal targets (attribute/derive only).
    pub applies_to: Vec<MacroTargetKind>,
    /// Procedural macros: the `expand(...) -> Syntax` compile-time function
    /// (boxed to keep `Decl` variants comparably sized).
    pub expand_fn: Option<Box<FunctionDecl>>,
    /// `trigger { field }`: auto-apply when a FIELD of a declaration carries
    /// an annotation matching the macro's name (property-wrapper shape).
    pub trigger_field: bool,
    /// `replace { true }`: output REPLACES the annotated declaration.
    pub replace: bool,
    /// Source range of the declaration.
    pub span: Span,
}

/// One function/annotation parameter declaration.
#[derive(Debug, Clone)]
pub struct ParamDecl {
    /// Annotations preceding the parameter.
    pub annotations: Vec<Annotation>,
    /// The parameter name.
    pub name: Symbol,
    /// The declared type, when written.
    pub type_expr: Option<TypeExprId>,
    /// The default value, when written.
    pub default_value: Option<ExprId>,
    /// Source range of the parameter.
    pub span: Span,
}

/// An `enum Name<T> { ... }` declaration.
#[derive(Debug, Clone)]
pub struct EnumDecl {
    /// Annotations preceding the enum.
    pub annotations: Vec<Annotation>,
    /// The enum name.
    pub name: Symbol,
    /// Generic type parameter names.
    pub type_params: Vec<Symbol>,
    /// The variants in source order.
    pub variants: Vec<EnumVariantDecl>,
    /// Set by the macro expander for `@Derive(Copy)`; carries the copyability
    /// assertion into semantics where every variant payload is verified.
    pub derive_copy: bool,
    /// Source range of the declaration.
    pub span: Span,
}

/// One variant of an enum declaration.
#[derive(Debug, Clone)]
pub struct EnumVariantDecl {
    /// The variant name.
    pub name: Symbol,
    /// The payload type, when written.
    pub associated_type: Option<TypeExprId>,
    /// The default value, when written.
    pub default_value: Option<ExprId>,
    /// Source range of the variant.
    pub span: Span,
}

/// A `class` / `struct` declaration.
#[derive(Debug, Clone)]
pub struct TypeDecl {
    /// Class vs struct.
    pub kind: TypeKind,
    /// Annotations preceding the declaration.
    pub annotations: Vec<Annotation>,
    /// The type name.
    pub name: Symbol,
    /// Parent types (`extends`, classes only).
    pub parents: Vec<QualifiedName>,
    /// The body members in source order.
    pub members: Vec<BodyMember>,
    /// Set by the macro expander for `@Derive(Copy)`; carries the copyability
    /// assertion into semantics where every field is verified.
    pub derive_copy: bool,
    /// Source range of the declaration.
    pub span: Span,
}

/// The two nominal type declaration kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TypeKind {
    /// A reference-semantics `class`.
    Class,
    /// A value-semantics `struct`.
    Struct,
}

/// A `construct Name { ... }` declaration.
#[derive(Debug, Clone)]
pub struct ConstructDecl {
    /// Annotations preceding the construct.
    pub annotations: Vec<Annotation>,
    /// True for `comptime construct`.
    pub is_comptime: bool,
    /// The construct name.
    pub name: Symbol,
    /// Parent constructs.
    pub parents: Vec<QualifiedName>,
    /// The older section-based body surface.
    pub sections: Vec<ConstructSection>,
    /// Direct SwiftUI-style members at the body's top level (e.g.
    /// `@Required let body: Widget`); empty when only sections are used.
    pub members: Vec<BodyMember>,
    /// Source range of the declaration.
    pub span: Span,
}

/// One named section of a construct body.
#[derive(Debug, Clone)]
pub struct ConstructSection {
    /// The section's written name.
    pub name: Symbol,
    /// The recognized section kind.
    pub kind: ConstructSectionKind,
    /// The section entries in source order.
    pub entries: Vec<ConstructSectionEntry>,
    /// Source range of the section.
    pub span: Span,
}

/// The recognized construct section kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ConstructSectionKind {
    /// `annotations { ... }`
    Annotations,
    /// `modifiers { ... }`
    Modifiers,
    /// `requires { ... }`
    Requires,
    /// `lifecycle { ... }`
    Lifecycle,
    /// `builder { ... }`
    Builder,
    /// `representation { ... }`
    Representation,
    /// `properties { ... }`
    Properties,
    /// Any other (custom) section name.
    Custom,
}

/// One entry of a construct section (full port of the Zig union).
#[derive(Debug, Clone)]
pub enum ConstructSectionEntry {
    /// An annotation spec (`annotations` section).
    AnnotationSpec(AnnotationSpec),
    /// A field declaration.
    FieldDecl(FieldDecl),
    /// A lifecycle hook.
    LifecycleHook(LifecycleHook),
    /// A required function signature (`requires` section).
    FunctionSignature(FunctionSignature),
    /// A typed property slot (`properties` section).
    PropertySchema(PropertySchemaField),
    /// A named content channel (`content` section).
    ContentChannel(ContentChannelSchema),
    /// A content-composition directive.
    ContentDirective(ContentDirective),
    /// A `content project` mapping.
    ContentProjection(ContentProjection),
    /// A named rule entry.
    NamedRule(NamedRule),
}

/// A content-composition directive on a construct's `content` section:
/// `content sealed`, `content refine { ... }`, `content passthrough`, or
/// `content project { ... }`.
#[derive(Debug, Clone, Copy)]
pub struct ContentDirective {
    /// Which directive was written.
    pub mode: ContentDirectiveMode,
    /// Source range of the directive.
    pub span: Span,
}

/// The content-composition directive modes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ContentDirectiveMode {
    /// `content sealed`
    Sealed,
    /// `content refine { ... }`
    Refine,
    /// `content passthrough`
    Passthrough,
    /// `content project { ... }`
    Project,
}

/// A `content project { local as Parent.channel }` mapping: the declaration
/// section named `local` fills `Parent`'s `channel`.
#[derive(Debug, Clone)]
pub struct ContentProjection {
    /// The local section name.
    pub local: Symbol,
    /// The parent construct receiving the content.
    pub target_construct: QualifiedName,
    /// The parent channel being filled.
    pub target_channel: Symbol,
    /// Source range of the mapping.
    pub span: Span,
}

/// A named content channel declared in `content { ... }`, e.g.
/// `head { accepts WebElement; count 0..1 }`.
#[derive(Debug, Clone)]
pub struct ContentChannelSchema {
    /// The channel name.
    pub name: Symbol,
    /// The accepted element type, when constrained.
    pub accepts: Option<QualifiedName>,
    /// The allowed element count, when constrained.
    pub count: Option<CountRange>,
    /// Source range of the channel.
    pub span: Span,
}

/// An inclusive lower bound and optional upper bound, written `min..max` or
/// `min..` (`0..` is `{0, None}`, `0..1` is `{0, Some(1)}`).
#[derive(Debug, Clone, Copy)]
pub struct CountRange {
    /// The inclusive lower bound.
    pub min: u32,
    /// The inclusive upper bound, when written.
    pub max: Option<u32>,
    /// Source range of the range.
    pub span: Span,
}

/// A typed slot in a construct's `properties { ... }` schema, e.g.
/// `required path: String`; `required` slots must be provided by every
/// construct-backed declaration.
#[derive(Debug, Clone)]
pub struct PropertySchemaField {
    /// True when every declaration must provide the property.
    pub required: bool,
    /// The property name.
    pub name: Symbol,
    /// The property type, when written.
    pub type_expr: Option<TypeExprId>,
    /// The default value, when written.
    pub default_value: Option<ExprId>,
    /// Source range of the slot.
    pub span: Span,
}

/// An annotation spec entry of a construct's `annotations` section.
#[derive(Debug, Clone)]
pub struct AnnotationSpec {
    /// The accepted annotation's name.
    pub name: QualifiedName,
    /// The annotation's value type, when written.
    pub type_expr: Option<TypeExprId>,
    /// The default value, when written.
    pub default_value: Option<ExprId>,
    /// Source range of the spec.
    pub span: Span,
}

/// A named rule entry: `name(args): Type = value` or `name { ... }`.
#[derive(Debug, Clone)]
pub struct NamedRule {
    /// The rule's name.
    pub name: QualifiedName,
    /// The rule arguments in source order.
    pub args: Vec<RuleArg>,
    /// The rule's type, when written.
    pub type_expr: Option<TypeExprId>,
    /// The rule's value, when written.
    pub value: Option<ExprId>,
    /// The rule's block body, when written.
    pub block: Option<Block>,
    /// Source range of the rule.
    pub span: Span,
}

/// One (optionally labeled, optionally valueless) rule argument.
#[derive(Debug, Clone)]
pub struct RuleArg {
    /// The argument label, when written.
    pub label: Option<Symbol>,
    /// The argument value, when written.
    pub value: Option<ExprId>,
    /// Source range of the argument.
    pub span: Span,
}

/// A construct-backed declaration form (`Widget Counter(count: Int) { ... }`).
#[derive(Debug, Clone)]
pub struct ConstructFormDecl {
    /// Annotations preceding the declaration.
    pub annotations: Vec<Annotation>,
    /// The backing construct's name.
    pub construct_name: QualifiedName,
    /// The declared form's name.
    pub name: Symbol,
    /// The form's parameters in source order.
    pub params: Vec<ParamDecl>,
    /// The form's body.
    pub body: ConstructBody,
    /// Source range of the declaration.
    pub span: Span,
}

/// The body of a construct-backed declaration form.
#[derive(Debug, Clone)]
pub struct ConstructBody {
    /// The body members in source order.
    pub members: Vec<BodyMember>,
    /// Source range of the body.
    pub span: Span,
}

/// The quoted source of a `FailTest`: the parser captures the text verbatim
/// and the enclosing package's semantics never sees its contents.
#[derive(Debug, Clone)]
pub enum FailTestSource {
    /// Raw source text captured verbatim from a `source { ... }` block.
    Block(String),
    /// The decoded value of a `source = "..."` string literal (raw-string
    /// tier, for sources that must not even tokenize/brace-balance).
    String(String),
}

/// A `FailTest Name { backends {...} source {...} expect {...} }` declaration:
/// an expected-compile-outcome test written in pure Kira; the `kira test`
/// runner compiles the quoted source per declared backend and matches the
/// outcome against the `expect` block.
#[derive(Debug, Clone)]
pub struct FailTestDecl {
    /// Annotations preceding the declaration.
    pub annotations: Vec<Annotation>,
    /// The test name.
    pub name: Symbol,
    /// Declared backends (lowercase idents from `backends { ... }`), a subset
    /// of {vm, llvm, hybrid}; empty means the default: vm only.
    pub backends: Vec<Symbol>,
    /// The quoted source to compile, when the `source` section is present.
    pub source: Option<FailTestSource>,
    /// Raw text of the `expect { ... }` block, for textual extraction of the
    /// expected diagnostic code and Ok/Error polarity.
    pub expect_text: Option<String>,
    /// Source range of the declaration.
    pub span: Span,
}

/// One member of a type/construct/extend body (full port of the Zig union).
#[derive(Debug, Clone)]
pub enum BodyMember {
    /// A field declaration.
    FieldDecl(FieldDecl),
    /// A function declaration.
    FunctionDecl(FunctionDecl),
    /// A `content { ... }` builder section.
    ContentSection(ContentSection),
    /// A `properties { ... }` value section.
    PropertiesSection(DeclPropertiesSection),
    /// A lifecycle hook.
    LifecycleHook(LifecycleHook),
    /// A named rule.
    NamedRule(NamedRule),
}

/// A construct-backed declaration's `properties { path: "/" }` section: each
/// entry binds a schema property name to a value expression.
#[derive(Debug, Clone)]
pub struct DeclPropertiesSection {
    /// The property bindings in source order.
    pub entries: Vec<DeclPropertyEntry>,
    /// Source range of the section.
    pub span: Span,
}

/// One `name: value` binding of a declaration properties section.
#[derive(Debug, Clone)]
pub struct DeclPropertyEntry {
    /// The bound schema property.
    pub name: Symbol,
    /// The bound value.
    pub value: ExprId,
    /// Source range of the binding.
    pub span: Span,
}

/// A field declaration: stored state, or a block-bodied computed member.
#[derive(Debug, Clone)]
pub struct FieldDecl {
    /// Annotations preceding the field.
    pub annotations: Vec<Annotation>,
    /// True for `override let/var`.
    pub is_override: bool,
    /// `let` (immutable) vs `var` (mutable).
    pub storage: FieldStorage,
    /// The field name.
    pub name: Symbol,
    /// The declared type, when written.
    pub type_expr: Option<TypeExprId>,
    /// The initializer, when written.
    pub value: Option<ExprId>,
    /// A block body for a computed member (e.g. `let node: Node { body.node }`);
    /// when present the field is computed and `value` is `None`.
    pub body: Option<Block>,
    /// Source range of the field.
    pub span: Span,
}

/// Field/binding mutability.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FieldStorage {
    /// `let` — immutable.
    Immutable,
    /// `var` — mutable.
    Mutable,
}

/// A `content { ... }` section of a declaration body.
#[derive(Debug, Clone)]
pub struct ContentSection {
    /// Annotations preceding the section.
    pub annotations: Vec<Annotation>,
    /// The builder block producing the content.
    pub builder: BuilderBlock,
    /// Source range of the section.
    pub span: Span,
}

/// A lifecycle hook like `onAppear() { ... }`.
#[derive(Debug, Clone)]
pub struct LifecycleHook {
    /// The hook name.
    pub name: Symbol,
    /// The hook arguments in source order.
    pub args: Vec<RuleArg>,
    /// The hook body.
    pub body: Block,
    /// Source range of the hook.
    pub span: Span,
}
