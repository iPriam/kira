//! Top-level items: functions, structs, enums, their members, and the written
//! type references they name.
//!
use super::{Block, ExprId, ReceiverDecl, TraitDecl, TraitRef, TypeRefId};
use crate::ownership::OwnershipMode;
use kira_core::Symbol;
use kira_runtime_abi::Execution;
use kira_source::Span;

mod ffi;
mod types;

pub use ffi::{FfiTypeKind, FfiTypeMark, ForeignField, ForeignKind, ForeignMark};
pub use types::TypeRef;

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
    /// A `let Name: T = value` constant at module scope.
    Constant(ConstantDecl),
    /// An `import Module [as Alias]` declaration.
    Import(ImportDecl),
    /// A `construct Family { ... }` declaration family, or a construct-backed
    /// `construct Name(params) extends Family { ... }` declaration that conforms
    /// to one.
    Construct(ConstructDecl),
    /// An `extend Family { function ... }` block: fluent modifier methods added
    /// to a construct family's chainable surface, or an `extend T: Trait { … }`
    /// block: the implementation of a trait for a type declared elsewhere.
    Extend(ExtendDecl),
    /// A `trait Name { ... }` declaration.
    Trait(TraitDecl),
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
    /// The construct family being extended (`Widget`), or the type a trait is
    /// being implemented for when [`conforms`](Self::conforms) is present.
    pub name: Symbol,
    /// Span of the family name, for diagnostics and definition links.
    pub name_span: Span,
    /// A written type reference when the impl target is not an identifier,
    /// such as `extend [Int]: Equatable`. Named targets keep `None` for the
    /// compact family and type lookup paths.
    pub target: Option<TypeRefId>,
    /// The trait this block implements, when the header wrote `: Trait`.
    ///
    /// `Some` turns the block from a fluent modifier block into an **impl**:
    /// its members are the trait's members for the named type, and the block
    /// may add conformance only — never a parent, which is why this is one
    /// trait rather than the two-clause header a declaration writes.
    pub conforms: Option<TraitRef>,
    /// The modifier methods, in declaration order. Each is an ordinary
    /// [`Function`]; what makes it a modifier is the block it was written in —
    /// analysis binds `self` to the family value.
    pub methods: Vec<Function>,
    /// Span covering the whole `extend` block.
    pub span: Span,
}

/// A member of the `construct` declaration family: either a family template
/// (`construct Family { ... }`) or a construct-backed declaration
/// (`construct Name(params) extends Family { ... }`) that conforms to one.
///
/// One keyword for both, and the parameter list is what tells them apart: a
/// construct with one is a declaration whose parameters are its construction
/// inputs, and a construct without one is the template itself.
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
    /// The type parameters the header wrote, when any did.
    ///
    /// Recorded so semantics can refuse a generic construct with its own typed
    /// diagnostic rather than the parser guessing at what one would mean.
    pub type_params: Vec<TypeParamDecl>,
    /// The traits the declaration's `: Trait, Trait` clause named.
    pub traits: Vec<TraitRef>,
    /// The stored members: `@Required let`, plain `let`, and defaulted `let`.
    pub fields: Vec<ConstructField>,
    /// The behaviour members: computed block-bodied bridges (`let node: Any { … }`,
    /// each a zero-argument method read as a property) and `function` members.
    pub methods: Vec<ConstructMethod>,
    /// The secondary initializers: `init(…) { … }` members, in written order.
    ///
    /// The parenthesized header is the declaration's **primary** way to be
    /// constructed — the one that fills its stored members directly. Each `init`
    /// is another way, told apart from the primary and from each other by what
    /// it takes, and its body ends in a construction of this same declaration.
    ///
    /// They are functions returning the declaration, so they carry no receiver:
    /// an initializer runs to produce a value rather than on one.
    pub inits: Vec<Function>,
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
    /// `construct Name(params) extends Family { ... }` — a declaration backed
    /// by a family.
    Backed {
        /// The family this declaration conforms to.
        family: Symbol,
        /// Span of the family name, for diagnostics.
        family_span: Span,
        /// The construction inputs, written as a function-style param list.
        params: Vec<Param>,
    },
}

/// One stored member of a [`ConstructDecl`] (`@Required let`, plain `let`/`var`,
/// or `let name: Any = default`).
#[derive(Debug, Clone, PartialEq)]
pub struct ConstructField {
    /// The member's name.
    pub name: Symbol,
    /// Span of the name token.
    pub name_span: Span,
    /// Whether the member carried `@Required`: a value every backed declaration
    /// must provide.
    pub required: bool,
    /// Whether a backed declaration may write through this member.
    ///
    /// Construct fields use the same `let`/`var` rule as struct fields. A
    /// required member is always spelled `@Required let`; a declaration can
    /// discharge it with a mutable `var` field when its implementation needs
    /// to update the value through a mutating receiver.
    pub mutable: bool,
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

/// A `let Name: T = value` written at module scope.
///
/// One value, computed once for the program and shared by every reader. That is
/// the whole point of it: a library that derives something from its own data — a
/// parsed table, a decoded resource — otherwise has to derive it again on every
/// call, because a function's parameter default and a struct's field default are
/// both re-evaluated per use and nothing else in the language holds a value for
/// longer than a frame.
///
/// It is immutable, so there is no `var` at module scope: a mutable one would
/// need an initialization order that is observable, and shared mutable state
/// across a program is not a thing this language is going to grow by accident.
///
/// The initializer is an ordinary expression evaluated at module load, in an
/// order that puts each constant after the ones it reads. A cycle among them has
/// no first value and is refused.
#[derive(Debug, Clone, PartialEq)]
pub struct ConstantDecl {
    /// The constant's name.
    pub name: Symbol,
    /// Span of the name token, for diagnostics.
    pub name_span: Span,
    /// The written type, when the declaration spelled one.
    ///
    /// Optional for the same reason a local's is: the initializer usually says
    /// the type already, and repeating it is noise.
    pub declared_type: Option<TypeRefId>,
    /// The initializer.
    pub value: ExprId,
    /// Span covering the whole declaration.
    pub span: Span,
}

/// One declared type parameter: `Value` in `enum Result<Value, Failure>`.
///
/// A parameter may carry a **bound** (`Value: Scored`), written after its name
/// as one or more trait names joined by `+`. A bound states what every type
/// argument for this parameter must conform to; it is discharged when an
/// instantiation is minted — and inside a generic body it is what makes the
/// bound trait's members callable on a value of the parameter. See
/// `kira-semantics`'s generics module.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeParamDecl {
    /// The parameter's name.
    pub name: Symbol,
    /// Span of the name token, for diagnostics.
    pub span: Span,
    /// The traits every type argument must conform to, in written order.
    ///
    /// Empty for the ordinary unbounded parameter; an unbounded parameter
    /// carries no restriction and no capability beyond being a name the
    /// template's body can substitute into.
    pub bounds: Vec<TraitRef>,
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
/// each written instantiation is what mints one. The struct, class, function,
/// and trait forms take type parameters the same way.
#[derive(Debug, Clone, PartialEq)]
pub struct EnumDecl {
    /// The enum's name.
    pub name: Symbol,
    /// Span of the name token, for diagnostics.
    pub name_span: Span,
    /// The traits the declaration claims to implement.
    pub traits: Vec<TraitRef>,
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
    /// The declared type parameters, in order; empty for an ordinary struct.
    ///
    /// A generic struct names no type by itself: each written instantiation
    /// (`Box<Int>`) is what declares one, with the arguments substituted into
    /// the fields. See `kira-semantics`'s generics module for the model, which
    /// is the one the other declaration forms share.
    pub type_params: Vec<TypeParamDecl>,
    /// The traits the declaration's `: Trait, Trait` clause named.
    pub traits: Vec<TraitRef>,
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
    /// The declared type parameters, in order; empty for an ordinary class.
    ///
    /// A generic class instantiates exactly as a generic struct does — see
    /// [`StructDecl::type_params`].
    pub type_params: Vec<TypeParamDecl>,
    /// The traits the declaration's `: Trait, Trait` clause named.
    pub traits: Vec<TraitRef>,
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
    /// Explicit type arguments on the parent, as in `extends Parent<Value>`.
    /// Empty for an ordinary parent or for a malformed/incomplete list.
    pub type_args: Vec<TypeRefId>,
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

/// A function declaration: signature plus body.
#[derive(Debug, Clone, PartialEq)]
pub struct Function {
    /// The function's name.
    pub name: Symbol,
    /// Span of the name token, for diagnostics.
    pub name_span: Span,
    /// The declared type parameters, in order; empty for an ordinary function.
    ///
    /// A generic function names no callable by itself: each call is what
    /// declares one, with the type arguments inferred from the value arguments
    /// (or written explicitly) substituted into the signature and the body.
    pub type_params: Vec<TypeParamDecl>,
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
    /// The written `self` receiver, when the declaration spelled one.
    ///
    /// A method that writes none still has a receiver — every method does, and
    /// it borrows — so this records only what was *written*, which is the one
    /// thing the mode cannot be inferred from: `borrow mut self` says the body
    /// writes through the value it was called on even where no statement in it
    /// does yet.
    pub receiver: Option<ReceiverDecl>,
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

/// A parsed-but-unanalyzed top-level construct.
#[derive(Debug, Clone, PartialEq)]
pub struct UnsupportedItem {
    /// A short label naming the construct (`"struct"`, `"import"`, …).
    pub keyword: &'static str,
    /// Span covering the construct.
    pub span: Span,
}
