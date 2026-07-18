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
    /// An `enum` declaration: a tagged union of named variants.
    Enum(EnumDecl),
    /// A construct the v0 subset parses but does not yet analyze (class,
    /// import, …); recorded so semantics can report it cleanly.
    Unsupported(UnsupportedItem),
}

/// An `enum` declaration: a named set of variants, each optionally carrying a
/// single payload value.
///
/// Variants are separated by newlines or spaces — never commas — so the variant
/// name is what starts each one. A variant may carry a payload written either
/// `Name(Type)` or `Name: Type = default`; the second form supplies a default
/// used when the variant is constructed with no explicit payload.
#[derive(Debug, Clone, PartialEq)]
pub struct EnumDecl {
    /// The enum's name.
    pub name: Symbol,
    /// Span of the name token, for diagnostics.
    pub name_span: Span,
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
    /// An array type: `[Int]`.
    Array {
        /// The written element type.
        element: TypeRefId,
        /// Span covering the brackets and their contents.
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
            TypeRef::Named { span, .. } | TypeRef::Array { span, .. } | TypeRef::Error { span } => {
                *span
            }
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
