//! Top-level items: functions, structs, enums, their members, and the written
//! type references they name.

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
    /// A construct the v0 subset parses but does not yet analyze (class,
    /// import, …); recorded so semantics can report it cleanly.
    Unsupported(UnsupportedItem),
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
/// It carries no type, because it declares no field — the inherited field's
/// slot and type are what it rebinds. That is why this is its own node rather
/// than a [`FieldDecl`] with a flag.
#[derive(Debug, Clone, PartialEq)]
pub struct OverrideFieldDecl {
    /// The inherited member's name.
    pub name: Symbol,
    /// Span of the name token.
    pub name_span: Span,
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
    /// Whether the declaration carried the `@Main` annotation.
    pub is_main: bool,
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
    /// A function type: `(Int) -> Void`, `() -> Void`, `(Int, Int) -> Int`.
    ///
    /// The result is always written — `->` and a type — because a function type
    /// has no "absent means `Void`" spelling the way a declaration does.
    Function {
        /// The written parameter types, in order; empty for `() -> R`.
        params: Vec<TypeRefId>,
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
