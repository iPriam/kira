//! Top-level items: functions, structs, enums, their members, and the written
//! type references they name.
//!
//! # Why this file stays whole past the size ladder
//!
//! It is 27 node definitions and four methods: a grammar written down, not
//! logic that grew. [`Item`] enumerates every variant, and each variant's node
//! sits below it, so the file reads top-down in the order the grammar does.
//! Splitting it would put an [`Item`] variant in one file and its payload in
//! another, and the split would have to fall somewhere — items versus members,
//! or aggregates versus functions — that the grammar does not actually divide:
//! a [`ClassDecl`] holds [`FieldDecl`]s, [`OverrideFieldDecl`]s, and
//! [`ClassMethod`]s, each of which holds a [`Function`]. Every consumer matches
//! on [`Item`] and walks straight through, so nobody would import one half.
//!
//! The ladder's concern is a file where behavior accumulates until no one can
//! hold it in mind. Nothing here has behavior to accumulate; the file grows only
//! when the language gains syntax, and then it grows by a node.

use super::{Block, ExprId, TypeRefId};
use crate::ownership::OwnershipMode;
use kira_core::Symbol;
use kira_runtime_abi::Execution;
use kira_source::Span;

/// A top-level declaration.
#[derive(Debug, Clone, PartialEq)]
pub enum Item {
    /// A function declaration.
    Function(Function),
    /// A `struct` declaration: a non-inheriting value shape.
    Struct(StructDecl),
    /// A `class` declaration: a value shape that inherits members.
    Class(ClassDecl),
    /// An `enum` declaration: a tagged union of named variants.
    Enum(EnumDecl),
    /// A `type Name = Target` alias.
    TypeAlias(TypeAliasDecl),
    /// An `import Module [as Alias]` declaration.
    Import(ImportDecl),
    /// A `construct Family { ... }` declaration family, or a construct-backed
    /// `Family Name(params) { ... }` declaration that conforms to one.
    Construct(ConstructDecl),
    /// An `extend Family { function ... }` block: fluent modifier methods added
    /// to a construct family's chainable surface.
    Extend(ExtendDecl),
    /// A construct the v0 subset parses but does not yet analyze (class,
    /// import, …); recorded so semantics can report it cleanly.
    Unsupported(UnsupportedItem),
}

/// An `extend Family { function ... }` block.
///
/// New Kira design proven against the oracle's *meaning*: the oracle documents
/// `extend` as validate-only (modifier bodies are checked, never lowered).
/// Here each modifier lowers to one real function whose receiver is the family
/// value (`some Family`) — so a fluent chain (`text.padding(8).background(fill)`)
/// runs on every backend. A modifier returns the family type and wraps the
/// receiver via `self`.
#[derive(Debug, Clone, PartialEq)]
pub struct ExtendDecl {
    /// The construct family being extended (`Widget`).
    pub name: Symbol,
    /// Span of the family name, for diagnostics and definition links.
    pub name_span: Span,
    /// The modifier methods, in declaration order. Each is an ordinary
    /// [`Function`]; what makes it a modifier is the block it was written in —
    /// analysis binds `self` to the family value.
    pub methods: Vec<Function>,
    /// Span covering the whole `extend` block.
    pub span: Span,
}

/// A member of the `construct` declaration family: either a family template
/// (`construct Family { ... }`) or a construct-backed declaration
/// (`Family Name(params) { ... }`) that conforms to one.
///
/// New Kira design proven against the oracle's *meaning*, not its state: the
/// oracle documents the construct family as validate-only ("construct-backed
/// declarations do not execute yet"). Here a construct-backed declaration is a
/// typed factory — it lowers to a class-shaped struct whose fields are the
/// declared params and whose computed members are zero-argument methods — so
/// constructing it and reading its bridge member runs on every backend.
#[derive(Debug, Clone, PartialEq)]
pub struct ConstructDecl {
    /// Whether this is the family template or a declaration backed by one.
    pub kind: ConstructKind,
    /// The declaration's own name (`Widget` for the family, `Text` for a
    /// backed declaration).
    pub name: Symbol,
    /// Span of the name token, for diagnostics.
    pub name_span: Span,
    /// The stored members: `@Required let`, plain `let`, and defaulted `let`.
    pub fields: Vec<ConstructField>,
    /// The behaviour members: computed block-bodied bridges (`let node: Any { … }`,
    /// each a zero-argument method read as a property) and `function` members.
    pub methods: Vec<ConstructMethod>,
    /// The families this one extends, in written order.
    ///
    /// A family that extends another adds its parent's requirements and members
    /// to its own surface, and every declaration backed by it also becomes a
    /// variant of the parent's family type. That is what lets a runtime hold
    /// `[Parent]` and drive declarations written against any child.
    pub extends: Vec<ConstructParent>,
    /// Members and clauses parsed but not yet executable, kept so semantics can
    /// refuse each with a precise typed diagnostic rather than dropping it.
    pub deferred: Vec<DeferredConstruct>,
    /// Span covering the whole declaration.
    pub span: Span,
}

/// One family named in a construct's `extends` clause.
#[derive(Debug, Clone, PartialEq)]
pub struct ConstructParent {
    /// The parent family's name.
    pub name: Symbol,
    /// Span of the name token, for diagnostics.
    pub span: Span,
}

/// One behaviour member of a [`ConstructDecl`].
///
/// A computed member (`let node: Any { … }`) and a `function` member are the same
/// runtime shape — a method whose receiver is the backed declaration — so they
/// share this node. What differs is the read: a computed member is read as a
/// property (`value.node`), a `function` member is called (`value.lower(ctx)`).
#[derive(Debug, Clone, PartialEq)]
pub struct ConstructMethod {
    /// Whether this came from a computed `let name: Any { … }` member, which is
    /// read as a property rather than called. The bridge member (`node`) is the
    /// canonical one.
    pub computed: bool,
    /// Whether this came from a `@Required function name(…) -> T` member: a
    /// **bodyless** signature the family declares and every backed declaration
    /// must implement itself.
    ///
    /// [`function`](Self::function) then carries an empty body, which is not a
    /// body to inherit — it is the absence of one. A backed declaration that
    /// leaves the member unimplemented has no implementation at all, and that is
    /// what the conformance check reports.
    pub required: bool,
    /// Whether this came from the family's `lifecycle { … }` section.
    ///
    /// A hook is an ordinary instance method — that is the whole point, so a
    /// runtime holding a declaration's value can call it. What this records is
    /// only that the family named it as a lifecycle point, which is how a
    /// runtime finds the hooks it is meant to drive without knowing any hook by
    /// name.
    pub lifecycle: bool,
    /// Whether the hook carried `@Comptime`: it runs during compilation rather
    /// than being called by a runtime.
    pub comptime: bool,
    /// The method itself: a zero-argument function for a computed member, or the
    /// written signature for a `function` member.
    pub function: Function,
}

/// Whether a [`ConstructDecl`] is the family template or a backed declaration.
#[derive(Debug, Clone, PartialEq)]
pub enum ConstructKind {
    /// `construct Family { ... }` — a declaration family (a typed template).
    Family,
    /// `Family Name(params) { ... }` — a declaration backed by a family.
    Backed {
        /// The family this declaration conforms to.
        family: Symbol,
        /// Span of the family name, for diagnostics.
        family_span: Span,
        /// The construction inputs, written as a function-style param list.
        params: Vec<Param>,
    },
}

/// One stored member of a [`ConstructDecl`] (`@Required let`, plain `let`, or
/// `let name: Any = default`).
#[derive(Debug, Clone, PartialEq)]
pub struct ConstructField {
    /// The member's name.
    pub name: Symbol,
    /// Span of the name token.
    pub name_span: Span,
    /// Whether the member carried `@Required`: a value every backed declaration
    /// must provide.
    pub required: bool,
    /// Whether this is a **child slot**: a field whose type was written
    /// `some X` / `[some X]`, or that carried the compat `@Content` annotation.
    ///
    /// A slot is filled at a construction site by content, never by a
    /// positional argument: the bare children of the trailing `{ … }` block
    /// fill the first slot, and a named fill (`detail: { … }`, `detail: View()`)
    /// fills the slot it names. A slot may declare a default, which stands in
    /// when nothing filled it. [`ty`](ConstructField::ty) carries the inner element type (`X`
    /// for `some X`, `[X]` for `[some X]`), so a single slot and a list slot are
    /// told apart by whether the type is an array.
    pub slot: bool,
    /// The declared member type, when the member wrote one.
    ///
    /// A stored member may omit the annotation when it has an initializer; the
    /// semantic pass then infers the field type from that initializer. Slots
    /// and computed members still require a type because their value is not an
    /// ordinary stored initializer.
    pub ty: Option<TypeRefId>,
    /// The default initializer, when one was written.
    pub default: Option<ExprId>,
    /// Span covering the whole member.
    pub span: Span,
}

/// A construct-family member or clause parsed but not yet executable.
///
/// Recorded so semantics refuses it with a precise typed diagnostic — never
/// silently, and never as the generic parse-don't-crash node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeferredConstruct {
    /// A short label naming the feature (`@Content slot`, `extends`, …).
    pub label: &'static str,
    /// Span of the not-yet-executable member or clause.
    pub span: Span,
}

/// An `import Module [as Alias]` declaration.
///
/// Imports are **file-scoped**: an import written in one file says nothing
/// about its siblings, so the declaration is recorded as an ordinary item and
/// the file it came from — which [`SyntaxTree`](super::SyntaxTree) tracks
/// alongside every item — is what gives it its scope.
///
/// The module path is dotted (`Foundation.Web`), stored one segment per
/// element; a single-segment path is the common case. The alias is the name the
/// file spells the module's namespace root with; when none is written the last
/// segment serves.
#[derive(Debug, Clone, PartialEq)]
pub struct ImportDecl {
    /// The dotted module path, one symbol per segment. Never empty for a
    /// well-formed import.
    pub path: Vec<Symbol>,
    /// Span covering the whole dotted path, for diagnostics about the module.
    pub path_span: Span,
    /// The `as Alias` name, when one was written.
    pub alias: Option<Symbol>,
    /// Span of the alias token, when one was written.
    pub alias_span: Option<Span>,
    /// Span covering the whole declaration.
    pub span: Span,
}

/// A `type Name = Target` alias: a second spelling for one written type.
///
/// The target is an ordinary [`TypeRef`], so an alias names anything a type
/// position can — a builtin, a struct, an enum, an array, or another alias.
/// Nothing below semantics learns aliases exist: analysis resolves the name to
/// the type it stands for, and the HIR carries
/// the target as if it had been written out.
#[derive(Debug, Clone, PartialEq)]
pub struct TypeAliasDecl {
    /// The alias's name.
    pub name: Symbol,
    /// Span of the name token, for diagnostics.
    pub name_span: Span,
    /// The written target type.
    pub target: TypeRefId,
    /// Span covering the whole declaration.
    pub span: Span,
}

/// One declared type parameter: `Value` in `enum Result<Value, Failure>`.
///
/// A type parameter is a *name only* — this language has no bounds, no
/// defaults, and no variance annotations, so there is nothing else to record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeParamDecl {
    /// The parameter's name.
    pub name: Symbol,
    /// Span of the name token, for diagnostics.
    pub span: Span,
}

/// An `enum` declaration: a named set of variants, each optionally carrying a
/// single payload value.
///
/// Variants are separated by newlines or spaces — never commas — so the variant
/// name is what starts each one. A variant may carry a payload written either
/// `Name(Type)` or `Name: Type = default`; the second form supplies a default
/// used when the variant is constructed with no explicit payload.
///
/// A declaration may be *generic* (`enum Result<Value, Failure>`), in which
/// case `type_params` is non-empty and the declaration names no type by itself:
/// each written instantiation is what mints one. The enum is the only
/// declaration form that takes type parameters — a generic struct, class, or
/// function is refused at the parse, because nothing in the corpus writes one.
#[derive(Debug, Clone, PartialEq)]
pub struct EnumDecl {
    /// The enum's name.
    pub name: Symbol,
    /// Span of the name token, for diagnostics.
    pub name_span: Span,
    /// The declared type parameters, in order; empty for an ordinary enum.
    pub type_params: Vec<TypeParamDecl>,
    /// The variants, in declaration order.
    pub variants: Vec<VariantDecl>,
    /// The `@Derive(Copy)` assertion, when the declaration carried one. See
    /// [`StructDecl::derives_copy`].
    pub derives_copy: Option<Span>,
    /// Span covering the whole declaration.
    pub span: Span,
}

/// One variant of an [`EnumDecl`].
#[derive(Debug, Clone, PartialEq)]
pub struct VariantDecl {
    /// The variant's name.
    pub name: Symbol,
    /// Span of the name token.
    pub name_span: Span,
    /// The written payload type, when the variant carries one.
    pub payload: Option<TypeRefId>,
    /// The default payload initializer, when one was written (the `= expr`
    /// form). Only meaningful when `payload` is present.
    pub default: Option<ExprId>,
    /// Span covering the whole variant.
    pub span: Span,
}

/// A `struct` declaration: a named, non-inheriting value shape.
///
/// Members are written with `let` (immutable) or `var` (mutable) and may carry
/// a default initializer. Members are separated by newlines or `;` — the
/// parser treats both as insignificant, so the member keyword is what starts
/// each one.
#[derive(Debug, Clone, PartialEq)]
pub struct StructDecl {
    /// The struct's name.
    pub name: Symbol,
    /// Span of the name token, for diagnostics.
    pub name_span: Span,
    /// The stored members, in declaration order.
    pub fields: Vec<FieldDecl>,
    /// The methods declared in the body, in declaration order.
    ///
    /// A method is an ordinary [`Function`] here; what makes it a method is
    /// where it was written. Analysis is what gives it its receiver.
    pub methods: Vec<Function>,
    /// The `@FFI.*` type annotation the declaration carried, when one was
    /// written. Present for `@FFI.Struct`/`Pointer`/`Alias`/`Array`/`Callback`;
    /// absent for a plain struct. `@FFI.Extern` never rides a struct.
    pub ffi: Option<FfiTypeMark>,
    /// The `@Derive(Copy)` assertion, when the declaration carried one.
    ///
    /// `Copy` is a compiler builtin rather than a macro: it generates nothing
    /// and is an opt-in claim that the type is structurally copyable, checked
    /// where the field types are known. The span is the annotation's, so the
    /// refusal points at the claim rather than at the type.
    pub derives_copy: Option<Span>,
    /// Span covering the whole declaration.
    pub span: Span,
}

/// A `class` declaration: a value shape that inherits members from its parents.
///
/// A class differs from a [`StructDecl`] in exactly three ways: it names
/// parents, its members may be marked `override`, and its methods may call a
/// parent's version of a member by qualifying it (`ClsAccount.gross()`, which
/// is how this language spells "super"). Everything else about it is a struct,
/// and semantics flattens it into one.
///
/// The parent list is comma-separated (`extends ClsAlpha, ClsBeta`) and may
/// name a `struct` as readily as a `class`.
#[derive(Debug, Clone, PartialEq)]
pub struct ClassDecl {
    /// The class's name.
    pub name: Symbol,
    /// Span of the name token, for diagnostics.
    pub name_span: Span,
    /// The written parents, in declaration order. Empty when no `extends` was
    /// written — a class need not inherit.
    pub parents: Vec<ParentRef>,
    /// The members this class declares itself, in declaration order.
    pub fields: Vec<FieldDecl>,
    /// The `override let name = value` members, in declaration order.
    pub overrides: Vec<OverrideFieldDecl>,
    /// The methods declared in the body, in declaration order.
    pub methods: Vec<ClassMethod>,
    /// The `@Export` marker, when the declaration carried one.
    ///
    /// On a class the marker means *handle-eligible*: instances of an exported
    /// class may cross the export boundary as opaque handles. It exports no
    /// method — functions are the exported surface.
    pub export: Option<ExportMark>,
    /// Span covering the whole declaration.
    pub span: Span,
}

/// The `@Export` annotation on a declaration, plus any payload it carried.
///
/// `@Export` is **bare**: it takes no argument list and no block. The parser
/// still records a payload that was written rather than dropping it, so the
/// refusal can point at what the author typed instead of at the annotation
/// name.
///
/// New Kira design, not oracle behavior: the oracle has no export concept at
/// all. What it does pin is the annotation grammar this rides on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExportMark {
    /// Span of the `@Export` annotation name.
    pub span: Span,
    /// Span of an argument list or block written after `@Export`, when one was.
    pub payload_span: Option<Span>,
}

/// One name in a class's `extends` list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParentRef {
    /// The parent type's name.
    pub name: Symbol,
    /// Span of the name token, for diagnostics about this parent.
    pub span: Span,
}

/// An `override let name = value` member: a new default for an inherited field.
///
/// It declares no field — the inherited field's slot and type are what it
/// rebinds — which is why this is its own node rather than a [`FieldDecl`] with
/// a flag. Its type is therefore optional and, when written, a restatement
/// rather than a declaration; see [`Self::ty`].
#[derive(Debug, Clone, PartialEq)]
pub struct OverrideFieldDecl {
    /// The inherited member's name.
    pub name: Symbol,
    /// Span of the name token.
    pub name_span: Span,
    /// The restated field type, when the declaration wrote one.
    ///
    /// An override may name the inherited type for readability, and doing so
    /// changes nothing: the slot keeps the type it was declared with. It is
    /// checked rather than ignored, because a type that *disagrees* with the
    /// inherited one is a mistake about which field is being rebound.
    pub ty: Option<TypeRefId>,
    /// The replacement default. Required: an override with no value would say
    /// nothing.
    pub default: ExprId,
    /// Span covering the whole member.
    pub span: Span,
}

/// One method of a [`ClassDecl`], plus whether it was written `override`.
#[derive(Debug, Clone, PartialEq)]
pub struct ClassMethod {
    /// Whether the declaration carried the `override` keyword.
    pub is_override: bool,
    /// The method itself.
    pub function: Function,
}

/// One stored member of a [`StructDecl`].
#[derive(Debug, Clone, PartialEq)]
pub struct FieldDecl {
    /// The member's name.
    pub name: Symbol,
    /// Span of the name token.
    pub name_span: Span,
    /// `true` for `var`, `false` for `let`.
    pub mutable: bool,
    /// The declared member type.
    pub ty: TypeRefId,
    /// The default initializer, when one was written.
    pub default: Option<ExprId>,
    /// Span covering the whole member.
    pub span: Span,
}

/// One `key: value;` field inside an `@FFI.Extern { ... }` block.
///
/// Both the key (`library`, `symbol`, `abi`) and the value are written as bare
/// identifiers, so both are interned symbols. What each key means — and which
/// values a key accepts — is the analyzer's to decide; the parser only records
/// the `identifier : identifier ;` shape and the spans, so a later refusal can
/// point at the exact token the author wrote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForeignField {
    /// The field's key (`library`, `symbol`, `abi`).
    pub key: Symbol,
    /// Span of the key token, for diagnostics.
    pub key_span: Span,
    /// The field's value, written as a bare identifier (`kira_ffi_add`, `c`).
    pub value: Symbol,
    /// Span of the value token, for diagnostics.
    pub value_span: Span,
}

/// The parsed `@FFI.Extern { ... }` annotation on a bodyless function.
///
/// New Kira design: the oracle has no seamless C-FFI. The mark records the
/// annotation name's span, the block's span, and the `key: value;` fields as
/// written — nothing is validated here. The analyzer reads the fields, checks
/// the signature, and either mints a foreign callable or refuses the whole
/// declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForeignMark {
    /// Span of the qualified annotation name (`FFI.Extern`).
    pub span: Span,
    /// Span covering the whole `{ ... }` block.
    pub block_span: Span,
    /// The fields the block wrote, in source order.
    pub fields: Vec<ForeignField>,
}

/// A `@FFI.*` annotation on a *struct* declaration — every member of the family
/// except `@FFI.Extern`, which rides a function instead.
///
/// The five struct-attached forms each declare a *type* whose real shape the
/// annotation carries: `@FFI.Struct` a C-layout struct, `@FFI.Pointer` a native
/// pointer alias, `@FFI.Alias` a plain typedef, `@FFI.Array` an inline
/// fixed-size C array, and `@FFI.Callback` a function-pointer typedef. The
/// parser records the shape; the analyzer resolves the referenced types and
/// decides what each becomes.
#[derive(Debug, Clone, PartialEq)]
pub struct FfiTypeMark {
    /// Which of the five struct-attached `@FFI.*` forms this is, with its
    /// parsed arguments.
    pub kind: FfiTypeKind,
    /// Span of the qualified annotation name (`FFI.Struct`, `FFI.Pointer`, …).
    pub name_span: Span,
    /// Span covering the whole `{ ... }` block.
    pub block_span: Span,
}

/// The five struct-attached `@FFI.*` forms, each with the arguments its block
/// carried. A required argument the block omitted is recorded as `None`/empty,
/// so the analyzer reports the omission against the block rather than the parser
/// bailing.
#[derive(Debug, Clone, PartialEq)]
pub enum FfiTypeKind {
    /// `@FFI.Struct { layout: c; }` — a struct laid out by C rules. The
    /// declaration's own body carries the fields; this only records `layout`.
    Struct {
        /// The `layout` value as written (`c`), and its span.
        layout: Option<(Symbol, Span)>,
    },
    /// `@FFI.Pointer { target: Target; ownership: o; }` — a native pointer alias.
    Pointer {
        /// The written pointee type, when present.
        target: Option<TypeRefId>,
        /// The `ownership` value as written (`borrowed`), and its span.
        ownership: Option<(Symbol, Span)>,
    },
    /// `@FFI.Alias { target: Target; }` — a plain typedef of one type to another.
    Alias {
        /// The written aliased type, when present.
        target: Option<TypeRefId>,
    },
    /// `@FFI.Array { element: E; count: N; }` — an inline fixed-size C array.
    Array {
        /// The written element type, when present.
        element: Option<TypeRefId>,
        /// The written element count and its span, when present.
        count: Option<(i64, Span)>,
    },
    /// `@FFI.Callback { abi: c; params: [ParamType, …]; result: ResultType; }` — a
    /// function-pointer typedef.
    Callback {
        /// The `abi` value as written (`c`), and its span.
        abi: Option<(Symbol, Span)>,
        /// The written parameter types, in order; empty for `params: []`.
        params: Vec<TypeRefId>,
        /// The written result type, when present.
        result: Option<TypeRefId>,
    },
}

impl FfiTypeKind {
    /// A short label naming the form, for diagnostics (`Struct`, `Pointer`, …).
    pub fn label(&self) -> &'static str {
        match self {
            FfiTypeKind::Struct { .. } => "Struct",
            FfiTypeKind::Pointer { .. } => "Pointer",
            FfiTypeKind::Alias { .. } => "Alias",
            FfiTypeKind::Array { .. } => "Array",
            FfiTypeKind::Callback { .. } => "Callback",
        }
    }
}

/// A function declaration: signature plus body.
#[derive(Debug, Clone, PartialEq)]
pub struct Function {
    /// The function's name.
    pub name: Symbol,
    /// Span of the name token, for diagnostics.
    pub name_span: Span,
    /// Whether the declaration carried the `@Main` annotation.
    pub is_main: bool,
    /// Whether the declaration was written `async function`.
    ///
    /// `async` is contextual, not a keyword: it is an ordinary identifier
    /// everywhere else, and only the token immediately before `function` at a
    /// declaration's start reads as this marker.
    pub is_async: bool,
    /// The `@FFI.Extern` marker, when the declaration carried one.
    ///
    /// A function carrying this is bodyless (its [`body`](Function::body) is an
    /// empty block spanned at the terminating `;`) and names a foreign C symbol.
    /// The analyzer validates it and records a foreign callable; a caller
    /// invokes it as an ordinary Kira function.
    pub foreign: Option<ForeignMark>,
    /// The `@Export` marker, when the declaration carried one.
    ///
    /// On a top-level function this is the export itself: the function joins
    /// the library's consumer-facing surface. On a method it is refused —
    /// v1 exports free functions only.
    pub export: Option<ExportMark>,
    /// The engine the declaration selected with `@Runtime` / `@Native`.
    ///
    /// [`Execution::Inherited`] when neither was written — the syntax tree
    /// records what the source said, and leaves resolving the default to the
    /// build.
    pub execution: Execution,
    /// Declared parameters, in order.
    pub params: Vec<Param>,
    /// Declared return type, if written (absent means `Void`).
    pub return_type: Option<TypeRefId>,
    /// The function body.
    pub body: Block,
    /// Span covering the whole declaration.
    pub span: Span,
}

/// One declared function parameter.
#[derive(Debug, Clone, PartialEq)]
pub struct Param {
    /// The parameter name.
    pub name: Symbol,
    /// Span of the name token.
    pub name_span: Span,
    /// How the parameter takes its argument.
    ///
    /// [`OwnershipMode::Owned`] when the type was written bare — an owned
    /// parameter is the default, not a special case.
    pub ownership: OwnershipMode,
    /// Span of the written ownership prefix (`borrow mut`), absent when the
    /// type was bare. Diagnostics point here to say where a mode came from.
    pub ownership_span: Option<Span>,
    /// The declared parameter type, with any ownership prefix stripped.
    pub ty: TypeRefId,
    /// The default initializer, when one was written (`x: Int = 3`).
    ///
    /// A call may omit a trailing (or, when labeled, a middle) argument whose
    /// parameter declares one; semantics resolves the expression once in the
    /// declaring file and fills the omitted slot with it. Absent means the
    /// argument is mandatory.
    pub default: Option<ExprId>,
    /// Span covering the whole parameter.
    pub span: Span,
}

/// A written type reference, e.g. `Int`, `Point`, `[Int]`, or `[[Byte]]`.
///
/// An arena node rather than a flat `Copy` struct because an array type nests:
/// `[[Int]]`'s element is itself a written type. Following the index/arena law
/// — a [`TypeRefId`] into the tree's arena, never a `Box` — is what keeps this
/// free of the recursive-allocation-per-node cost and keeps the whole model
/// lifetime-free.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TypeRef {
    /// A named type: `Int`, `String`, `Point`.
    Named {
        /// The type name as an interned symbol.
        name: Symbol,
        /// Where the type name appears.
        span: Span,
    },
    /// An existential over a construct family: `some Widget`.
    ///
    /// Names "a value of some concrete declaration backing this family" — the
    /// heterogeneous family value, which is what a child slot holds and what a
    /// function returns when the concrete declaration is its own business.
    ///
    /// The family stays separate from ordinary nominal types in syntax so
    /// semantics can reject `some` applied to a struct, class, alias, or
    /// builtin instead of silently treating the qualifier as decoration. Bare
    /// `Widget` resolves to the same [`Type`](kira_semantics_model::Type), so
    /// this variant buys the *check*, not a distinct resolved type.
    SomeConstruct {
        /// The construct family's name, including any module qualifier.
        family: Symbol,
        /// Span of the family name alone, for definition links and diagnostics.
        family_span: Span,
        /// Span covering `some` through the family name.
        span: Span,
    },
    /// The result type a construct family declares for one of its members.
    ///
    /// Written by nobody: this is what the `name { … }` member shorthand
    /// desugars its result type to. The shorthand says "here is the body of the
    /// member the family calls `name`", and what that member *returns* is the
    /// family's to state — but the parser has no families, so it defers the
    /// question instead of guessing.
    ///
    /// Resolving it asks the named family what it declared for that member.
    /// A family that declared none falls back to the family type itself, which
    /// is what keeps a `body { … }` on a family that never mentions `body`
    /// meaning what it always did.
    ConstructMember {
        /// The construct family whose member this is, including any qualifier.
        family: Symbol,
        /// Span of the family name, for the diagnostic when it is not a family.
        family_span: Span,
        /// The member name the shorthand wrote.
        member: Symbol,
        /// Span covering the shorthand's member name.
        span: Span,
    },
    /// A generic instantiation: `Result<Int, AppError>`.
    ///
    /// Written only where a generic declaration is in scope. Semantics
    /// monomorphizes it — the arguments are substituted into the declaration's
    /// body and the result is declared as an ordinary nominal type — so nothing
    /// below semantics ever sees this node or learns that generics exist.
    Generic {
        /// The generic declaration's name.
        name: Symbol,
        /// Span of the name alone, for a diagnostic about the declaration.
        name_span: Span,
        /// The written type arguments, in order. Never empty: `Name<>` does not
        /// parse.
        args: Vec<TypeRefId>,
        /// Span covering the name through the closing `>`.
        span: Span,
    },
    /// An array type: `[Int]`.
    Array {
        /// The written element type.
        element: TypeRefId,
        /// Span covering the brackets and their contents.
        span: Span,
    },
    /// A function type: `(Int) -> Void`, `() -> Void`, `(borrow Event) -> Void`.
    ///
    /// The result is always written — `->` and a type — because a function type
    /// has no "absent means `Void`" spelling the way a declaration does.
    Function {
        /// The written parameter types, in order; empty for `() -> R`.
        params: Vec<TypeRefId>,
        /// The ownership mode written on each parameter, index-aligned with
        /// `params`; [`OwnershipMode::Owned`] where none was written.
        ///
        /// Kept rather than dropped because it is what an indirect call checks
        /// its arguments against. Dropping it makes every parameter owned, so a
        /// call through `(borrow Event) -> Void` demands `move` for a value the
        /// source declared `borrow` — the mode is invisible at run time and
        /// decisive at the ownership check.
        param_ownership: Vec<OwnershipMode>,
        /// The written result type.
        result: TypeRefId,
        /// Span covering the parameter list through the result.
        span: Span,
    },
    /// A type position the parser could not parse; recovery inserts this.
    ///
    /// A variant rather than a sentinel name, so analysis resolves it to
    /// `Type::Error` **silently**: the parser already said what was wrong, and
    /// a second "unknown type `<error>`" on top of it would name a type nobody
    /// wrote.
    Error {
        /// Span of the malformed type.
        span: Span,
    },
}

impl TypeRef {
    /// The span covering this type reference.
    pub fn span(&self) -> Span {
        match self {
            TypeRef::Named { span, .. }
            | TypeRef::SomeConstruct { span, .. }
            | TypeRef::ConstructMember { span, .. }
            | TypeRef::Generic { span, .. }
            | TypeRef::Array { span, .. }
            | TypeRef::Function { span, .. }
            | TypeRef::Error { span } => *span,
        }
    }
}

/// A parsed-but-unanalyzed top-level construct.
#[derive(Debug, Clone, PartialEq)]
pub struct UnsupportedItem {
    /// A short label naming the construct (`"struct"`, `"import"`, …).
    pub keyword: &'static str,
    /// Span covering the construct.
    pub span: Span,
}
