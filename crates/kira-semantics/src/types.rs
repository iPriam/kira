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
/// A field is the one position that can distinguish a *forward reference* from
/// an unknown name, because only there is declaration order a rule. Everywhere
/// else the two are the same mistake.
#[derive(Debug, Clone)]
pub(crate) enum NameContext {
    /// An ordinary type position: a parameter, a return type, an annotation.
    Ordinary,
    /// A field of the named aggregate.
    Field {
        /// What kind of aggregate declares this field.
        owner_kind: AggregateKind,
        /// The aggregate whose field this is.
        owner: String,
    },
}

/// The kind of aggregate a field belongs to, so a diagnostic can name it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AggregateKind {
    /// A `struct` declaration.
    Struct,
    /// A `class` declaration.
    Class,
}

impl AggregateKind {
    /// The keyword that introduces this kind, for use in a message.
    pub(crate) fn noun(self) -> &'static str {
        match self {
            AggregateKind::Struct => "struct",
            AggregateKind::Class => "class",
        }
    }
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
            // A generic instantiation resolves to the enum it monomorphizes
            // into — an ordinary declared type by the time anyone else looks.
            TypeRef::Generic {
                name,
                name_span,
                args,
                span,
            } => {
                // The written name resolves to the generic declaration whether
                // or not the instantiation itself goes through, so the link is
                // recorded first: a jump from `Result<Int, E>` lands on
                // `enum Result` even while the arguments are still wrong.
                let written = self.interner.resolve(name).to_owned();
                self.link_type_name(&written, name_span);
                self.resolve_generic_instantiation(name, name_span, &args, span, context)
            }
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
            // A function type is a *synthesized struct*: the closure desugar
            // turns `(Int) -> Void` into a value shape with a tag and the
            // captures of every closure literal that has this type. Resolving
            // it here is what puts it in every type position at once — a
            // parameter, a field, a `let` annotation, a return type.
            TypeRef::Function { params, result, .. } => {
                let resolved: Vec<Type> = params
                    .iter()
                    .map(|&param| self.resolve_type_in(param, context))
                    .collect();
                let result_ty = self.resolve_type_in(result, context);
                self.function_type(resolved, result_ty)
            }
            // The parser already said what was wrong here. Saying "unknown type
            // `<error>`" on top of it would name a type nobody wrote.
            TypeRef::Error { .. } => Type::Error,
        }
    }

    /// Strips a module qualifier off a written type name, or reports why the
    /// qualifier does not resolve.
    ///
    /// A module-qualified spelling (`Support.Point`) names the same type the
    /// module declares bare. Stripping the qualifier is the whole of it:
    /// top-level names are unique across a package, so `Support.Point` and
    /// `Point` cannot be two different types — what the qualifier buys is the
    /// file-scope check that this file actually imported `Support`.
    ///
    /// Returns `None` once that check has failed and been reported.
    pub(crate) fn strip_module_qualifier(&mut self, written: &str, span: Span) -> Option<String> {
        match written.split_once('.') {
            Some((root, member)) => {
                if self.module_for_root(root).is_none() {
                    if !self.report_unimported_root(root, span) {
                        self.emit(
                            span,
                            "KSEM050",
                            format!("unknown type `{written}`: `{root}` is not an imported module"),
                        );
                    }
                    return None;
                }
                Some(member.to_owned())
            }
            None => Some(written.to_owned()),
        }
    }

    /// Resolves one written type *name* to a builtin or a declared struct.
    fn resolve_named_type(
        &mut self,
        name: kira_core::Symbol,
        span: Span,
        context: &NameContext,
    ) -> Type {
        let written = self.interner.resolve(name).to_owned();
        let Some(text) = self.strip_module_qualifier(&written, span) else {
            return Type::Error;
        };
        // A type parameter binding beats everything: inside `Result`'s body,
        // `Value` is whatever the instantiation said it is. A parameter may not
        // shadow a builtin (`KSEM170` refuses that at the declaration), so this
        // order takes nothing away from a name that already means something.
        if let Some(ty) = self.bound_type_param(&text) {
            return ty;
        }
        if let Some(ty) = Type::from_name(&text) {
            return ty;
        }
        // Aliases come before the nominal tables and after the builtins: a
        // builtin name is never available to alias (collecting them rejects
        // that), and an alias name can collide with no struct or enum, so the
        // order decides nothing except how fast the common case is found.
        if let Some(ty) = self.resolve_alias_name(&text, context) {
            self.link_type_name(&text, span);
            return ty;
        }
        if let Some(id) = self.program.types.structs().lookup(&text) {
            self.link_type_name(&text, span);
            return Type::Struct(id);
        }
        if let Some(id) = self.program.types.enums().lookup(&text) {
            self.link_type_name(&text, span);
            return Type::Enum(id);
        }
        // A generic enum written bare is a different mistake from an unknown
        // name, and it has a different fix: write the type arguments.
        if self.is_generic_enum(&text) {
            self.emit(
                span,
                "KSEM172",
                format!(
                    "generic enum `{text}` needs its type arguments here (write \
                     `{text}<...>`)"
                ),
            );
            return Type::Error;
        }
        match context {
            NameContext::Field { owner_kind, owner } => {
                self.report_unknown_field_type(*owner_kind, owner, &text, span)
            }
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
    ///
    /// The two forward references are themselves different mistakes. A type
    /// declared later in *this* file is moved above the one that wants it. A
    /// type declared in another file cannot be moved at all: what fixes it is
    /// an `import`, which is what puts that file ahead of this one in the
    /// program's dependencies-first module order.
    fn report_unknown_field_type(
        &mut self,
        owner_kind: AggregateKind,
        owner: &str,
        text: &str,
        span: Span,
    ) {
        let declared_in = self
            .tree
            .items_with_source()
            .find_map(|(source, item)| match item {
                Item::Struct(other) if self.interner.resolve(other.name) == text => Some(source),
                Item::Class(other) if self.interner.resolve(other.name) == text => Some(source),
                _ => None,
            });
        let Some(source) = declared_in else {
            self.emit(span, "KSEM050", format!("unknown type `{text}`"));
            return;
        };
        let kind = owner_kind.noun();
        let message = match source == self.source {
            true => format!(
                "{kind} `{owner}` cannot hold a `{text}` because `{text}` is declared \
                 later in the file; move `{text}` above `{owner}`",
            ),
            false => format!(
                "{kind} `{owner}` cannot hold a `{text}` because `{text}` is declared in \
                 a file this one does not import; add the `import` that names it",
            ),
        };
        self.emit(span, "KSEM051", message);
    }
}
