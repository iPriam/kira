//! Resolving a *written* type ([`TypeRef`]) to a *resolved* one ([`Type`]).
//!
//! One recursion serves every type position — a parameter, a return type, a
//! `let` annotation, a struct field — because an array type nests and every
//! position can hold one. What differs between positions is only how an
//! unresolvable **name** is reported, which is what [`NameContext`] carries.

use kira_semantics_model::Type;
use kira_source::Span;
use kira_syntax_model::ast::{Item, TypeRef, TypeRefId};

use crate::analyze::Analyzer;

/// Where a type name was written, for the diagnostic when it does not resolve.
///
/// A struct field is the one position that can distinguish a *forward
/// reference* from an unknown name, because only there is declaration order a
/// rule. Everywhere else the two are the same mistake.
#[derive(Debug, Clone)]
pub(crate) enum NameContext {
    /// An ordinary type position: a parameter, a return type, an annotation.
    Ordinary,
    /// A field of the named struct.
    Field {
        /// The struct whose field this is.
        owner: String,
    },
}

impl Analyzer<'_> {
    /// Resolves a written type in an ordinary position.
    pub(crate) fn resolve_type_ref(&mut self, id: TypeRefId) -> Type {
        self.resolve_type_in(id, &NameContext::Ordinary)
    }

    /// Resolves a written type, reporting unresolvable names per `context`.
    ///
    /// The recursion is the whole point: `[[Point]]` resolves its element,
    /// which resolves *its* element, and each level interns against the same
    /// table — so `[[Point]]` written twice is one [`Type`].
    pub(crate) fn resolve_type_in(&mut self, id: TypeRefId, context: &NameContext) -> Type {
        match self.tree.type_ref(id).clone() {
            TypeRef::Named { name, span } => self.resolve_named_type(name, span, context),
            TypeRef::Array { element, .. } => {
                let element_ty = self.resolve_type_in(element, context);
                // An array of a type that did not resolve is itself an error.
                // Interning it would mint a row named `[<error>]` that a later
                // `[<error>]` would compare *equal* to — turning one unresolved
                // name into two unrelated types that type-check against each
                // other.
                if element_ty == Type::Error {
                    return Type::Error;
                }
                self.program.types.array_of(element_ty)
            }
            // The parser already said what was wrong here. Saying "unknown type
            // `<error>`" on top of it would name a type nobody wrote.
            TypeRef::Error { .. } => Type::Error,
        }
    }

    /// Resolves one written type *name* to a builtin or a declared struct.
    fn resolve_named_type(
        &mut self,
        name: kira_core::Symbol,
        span: Span,
        context: &NameContext,
    ) -> Type {
        let text = self.interner.resolve(name).to_owned();
        if let Some(ty) = Type::from_name(&text) {
            return ty;
        }
        if let Some(id) = self.program.types.structs().lookup(&text) {
            return Type::Struct(id);
        }
        if let Some(id) = self.program.types.enums().lookup(&text) {
            return Type::Enum(id);
        }
        match context {
            NameContext::Field { owner } => self.report_unknown_field_type(owner, &text, span),
            NameContext::Ordinary => self.emit(
                span,
                "KSEM050",
                format!(
                    "unknown type `{text}` (v0 supports Int, Float, Bool, String, Void, \
                     declared structs and enums, and arrays of those)"
                ),
            ),
        }
        Type::Error
    }

    /// Reports a field's unresolvable type, distinguishing a forward reference
    /// from an unknown name — they are different mistakes with different fixes.
    fn report_unknown_field_type(&mut self, owner: &str, text: &str, span: Span) {
        let declared_later = self.tree.items.iter().any(|item| match item {
            Item::Struct(other) => self.interner.resolve(other.name) == text,
            _ => false,
        });
        if declared_later {
            self.emit(
                span,
                "KSEM051",
                format!(
                    "struct `{owner}` cannot hold a `{text}` because `{text}` is declared \
                     later in the file; move `{text}` above `{owner}`",
                ),
            );
        } else {
            self.emit(span, "KSEM050", format!("unknown type `{text}`"));
        }
    }
}
