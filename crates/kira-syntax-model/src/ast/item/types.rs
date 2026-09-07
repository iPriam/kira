use super::super::TypeRefId;
use crate::ownership::OwnershipMode;
use kira_core::Symbol;
use kira_source::Span;

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
