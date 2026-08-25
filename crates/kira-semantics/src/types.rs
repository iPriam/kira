//! Resolving a *written* type ([`TypeRef`]) to a *resolved* one ([`Type`]).
//!
//! One recursion serves every type position — a parameter, a return type, a
//! `let` annotation, a struct field — because an array type nests and every
//! position can hold one. What differs between positions is only how an
//! unresolvable **name** is reported, which is what [`NameContext`] carries.

use kira_semantics_model::Type;
use kira_source::{SourceId, Span};
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

/// A written name with its module qualifier resolved and set aside.
///
/// The qualifier survives the split because it decides *which* declaration the
/// name means when more than one package declares it.
#[derive(Debug, Clone)]
pub(crate) struct QualifiedName {
    /// The name with any module qualifier removed.
    pub(crate) text: String,
    /// The module the qualifier named, or `None` for a bare name.
    pub(crate) qualifier: Option<SourceId>,
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
            // `some Family` is the existential over a construct family. It
            // resolves to exactly what bare `Family` resolves to — the
            // synthesized family enum — so the node exists to earn the check
            // below, not to name a second type.
            TypeRef::SomeConstruct {
                family,
                family_span,
                ..
            } => {
                let written = self.interner.resolve(family).to_owned();
                let Some(qualified) = self.split_module_qualifier(&written, family_span) else {
                    return Type::Error;
                };
                let name = qualified.text;
                match self.construct_family_type(&name) {
                    Some(id) => {
                        self.link_type_name(&name, family_span);
                        Type::Enum(id)
                    }
                    None => {
                        self.emit(
                            family_span,
                            "KSEM237",
                            format!(
                                "`some` requires a construct: `{name}` is not a declared \
                                 construct family. Write `some ConstructName`, or drop `some` \
                                 for a non-construct type."
                            ),
                        );
                        Type::Error
                    }
                }
            }
            // The `name { … }` member shorthand's result type: whatever the
            // family declared that member to be.
            TypeRef::ConstructMember {
                family,
                family_span,
                member,
                ..
            } => self.resolve_construct_member_type(family, family_span, member),
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
            TypeRef::Function {
                params,
                param_ownership,
                result,
                ..
            } => {
                let resolved: Vec<Type> = params
                    .iter()
                    .map(|&param| self.resolve_type_in(param, context))
                    .collect();
                let result_ty = self.resolve_type_in(result, context);
                self.function_type(resolved, param_ownership, result_ty)
            }
            // The parser already said what was wrong here. Saying "unknown type
            // `<error>`" on top of it would name a type nobody wrote.
            TypeRef::Error { .. } => Type::Error,
        }
    }

    /// Resolves the `name { … }` shorthand's result type against its family.
    ///
    /// The shorthand implements a member the family named, so the family is what
    /// says what that member returns: a `@Required function test() -> Any` makes
    /// `test { … }` return `Any`, and a `@Required let body: Widget` makes
    /// `body { … }` return `some Widget`. Neither is a special case of the other,
    /// and neither mentions a member name this compiler knows.
    ///
    /// A family that declares no such member — or declares it with no written
    /// type — falls back to the family type. That is the answer the shorthand
    /// always gave, and it is still the right one when the family has said
    /// nothing to override it.
    fn resolve_construct_member_type(
        &mut self,
        family: kira_core::Symbol,
        family_span: Span,
        member: kira_core::Symbol,
    ) -> Type {
        let written = self.interner.resolve(family).to_owned();
        let Some(qualified) = self.split_module_qualifier(&written, family_span) else {
            return Type::Error;
        };
        let name = qualified.text;
        let Some(enum_id) = self.construct_family_type(&name) else {
            self.emit(
                family_span,
                "KSEM237",
                format!("`{name}` is not a construct family"),
            );
            return Type::Error;
        };
        let member = self.interner.resolve(member).to_owned();
        let declared = self
            .construct_families
            .get(&name)
            .and_then(|info| info.member_types.get(&member))
            .copied();
        let Some((type_ref, declared_in)) = declared else {
            return Type::Enum(enum_id);
        };
        // Resolved against the *family's* file: `test() -> Result<Any, Failure>`
        // names types the family's imports bring in, which the file writing the
        // shorthand need never have imported.
        let outer = std::mem::replace(&mut self.source, declared_in);
        let resolved = self.resolve_type_ref(type_ref);
        self.source = outer;
        resolved
    }

    /// Splits a module qualifier off a written type name, or reports why the
    /// qualifier does not resolve.
    ///
    /// Top-level names are unique *within* a package, so `Support.Point` and
    /// `Point` name the same type whenever `Support` is a module of the package
    /// that wrote them. Across packages they need not: two packages may each
    /// declare a `Color`, and then `KiraUIFoundation.Color` is written precisely
    /// to say which one is meant. So the qualifier is kept rather than dropped —
    /// [`Analyzer::visible_struct_qualified`] resolves against the package that
    /// owns the module it names, and the file-scope check that this file
    /// actually imported the root happens here.
    ///
    /// Returns `None` once that check has failed and been reported.
    pub(crate) fn split_module_qualifier(
        &mut self,
        written: &str,
        span: Span,
    ) -> Option<QualifiedName> {
        match written.split_once('.') {
            Some((root, member)) => {
                let Some(module) = self.module_source_for_root(root) else {
                    if !self.report_unimported_root(root, span) {
                        self.emit(
                            span,
                            "KSEM050",
                            format!("unknown type `{written}`: `{root}` is not an imported module"),
                        );
                    }
                    return None;
                };
                Some(QualifiedName {
                    text: member.to_owned(),
                    qualifier: Some(module),
                })
            }
            None => Some(QualifiedName {
                text: written.to_owned(),
                qualifier: None,
            }),
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
        let Some(qualified) = self.split_module_qualifier(&written, span) else {
            return Type::Error;
        };
        let text = qualified.text.clone();
        // A type parameter binding beats everything: inside `Result`'s body,
        // `Value` is whatever the instantiation said it is. A parameter may not
        // shadow a builtin (`KSEM170` refuses that at the declaration), so this
        // order takes nothing away from a name that already means something.
        if let Some(ty) = self.bound_type_param(&text) {
            return ty;
        }
        if let Some(ty) = Type::from_name(&text) {
            // `CString` is the seam type: legal only as an `@FFI.Extern`
            // parameter. Every other position resolves it here with
            // `in_foreign_signature` false, so it is refused where it is
            // written — a local, a field, an ordinary parameter or result — and
            // becomes `Error` so nothing cascades on the invented value. A
            // foreign result, resolved with the flag set, escapes this and is
            // refused by the foreign pass with the ownership-specific message.
            if ty == Type::CString && !self.in_foreign_signature {
                self.emit(
                    span,
                    "KSEM176",
                    "`CString` may only appear as an `@FFI.Extern` parameter: it is a \
                     borrowed C string with no owned Kira representation. Use `String` \
                     for owned text, or `RawPtr` for an opaque handle.",
                );
                return Type::Error;
            }
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
        // A family's *name* is not one of its values. Naming the type takes
        // `Any Widget` or `some Widget`, which say that the value is one of the
        // declarations backing the family rather than the family itself.
        if self.construct_family_type(&text).is_some() {
            self.emit(
                span,
                "KSEM207",
                format!(
                    "`{text}` is a construct family, so it names no value on its own; write `Any {text}` for a value of some declaration backing it"
                ),
            );
            return Type::Error;
        }
        // A trait used as a type is an existential over its conformers: the
        // value carries which concrete type it was, and member calls dispatch
        // to that type's implementation. Compiler-known traits stay refused —
        // they state facts about one type's own members or body, and none of
        // them classifies values (`Drop` attaches a body; `Copyable`, `Send`,
        // and `Sync` are checked claims), so there is nothing for a value of
        // "any `Send`" to be.
        if crate::traits::is_builtin_trait(&text) {
            self.emit(
                span,
                "KSEM295",
                format!(
                    "`{text}` states something about one type's own members, so it does not \
                     classify values and has no existential form. Name the concrete type here."
                ),
            );
            return Type::Error;
        }
        if self.traits.contains_key(&text) {
            let Some(enum_id) = self.reserve_trait_existential(&text, span) else {
                return Type::Error;
            };
            self.link_type_name(&text, span);
            return Type::Enum(enum_id);
        }
        if let Some(id) = self.visible_struct_qualified(&qualified) {
            self.link_type_name(&text, span);
            return Type::Struct(id);
        }
        if let Some(id) = self.visible_enum_qualified(&qualified) {
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
                    "unknown type `{text}` (supported types include builtins, declared structs, \
                     enums, construct families, and arrays of those)"
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
