//! Declaration-time construct collection and two-phase family registration.

use std::collections::HashSet;

use kira_semantics_model::hir::HirExprId;
use kira_semantics_model::{EnumDef, FieldDef, StructDef, StructId, Type, VariantDef};
use kira_source::{SourceId, Span};
use kira_syntax_model::ast::{ConstructDecl, ConstructKind, Function, Item};

use super::{
    ConstructFamilyField, ConstructFamilyInfo, ConstructFamilyMethod, ConstructInfo,
    ConstructVariant, ContentSlot,
};
use crate::analyze::{Analyzer, Callable, FieldDefault};

impl<'a> Analyzer<'a> {
    /// Declares one empty enum header per construct family.
    ///
    /// Runs before ordinary structs resolve so a field may name `Widget` or
    /// `Any Widget`. Concrete variants are filled later, after every backed
    /// declaration has a struct id.
    pub(crate) fn collect_construct_family_headers(&mut self) {
        for (source, declaration) in self.family_declarations() {
            self.source = source;
            let name = self.interner.resolve(declaration.name).to_owned();
            if self.construct_families.contains_key(&name) {
                self.emit(
                    declaration.name_span,
                    "KSEM004",
                    format!("construct family `{name}` is already defined"),
                );
                continue;
            }
            let enum_name = format!("Any {name}");
            let Some(enum_id) = self.program.types.enums_mut().declare(EnumDef {
                name: enum_name,
                variants: Vec::new(),
            }) else {
                self.emit(
                    declaration.name_span,
                    "KSEM006",
                    format!("construct family type `Any {name}` is already defined"),
                );
                continue;
            };
            self.enum_defaults.push(Vec::new());

            let required: Vec<String> = declaration
                .fields
                .iter()
                .filter(|field| field.required)
                .map(|field| self.interner.resolve(field.name).to_owned())
                .collect();
            // A `@Required let` is a *value* obligation, so it is readable
            // through the family value the way a computed member is. It gets its
            // own map rather than joining `methods` because it has no AST
            // function behind it: what discharges it is the backed
            // declaration's choice of a stored field or a computed member, and
            // the dispatcher reads whichever that declaration chose. Only
            // required fields are here, because only those are guaranteed to
            // exist on every variant (`KSEM201` is what guarantees it).
            let field_members = required
                .iter()
                .map(|name| {
                    (
                        name.clone(),
                        ConstructFamilyField {
                            result: Type::Error,
                            dispatcher: None,
                        },
                    )
                })
                .collect();
            // What the family says each of its members is, kept as written so a
            // `name { … }` shorthand can ask before types are resolved. A field
            // and a method are both members a shorthand can implement, so both
            // go in; a method with no written result contributes nothing, which
            // is what makes the shorthand fall back to the family type.
            let mut member_types = std::collections::BTreeMap::new();
            for field in &declaration.fields {
                member_types.insert(
                    self.interner.resolve(field.name).to_owned(),
                    (field.ty, source),
                );
            }
            for method in &declaration.methods {
                if let Some(result) = method.function.return_type {
                    member_types.insert(
                        self.interner.resolve(method.function.name).to_owned(),
                        (result, source),
                    );
                }
            }
            let mut methods = std::collections::BTreeMap::new();
            for method in &declaration.methods {
                let method_name = self.interner.resolve(method.function.name).to_owned();
                if methods.contains_key(&method_name) {
                    self.emit(
                        method.function.name_span,
                        "KSEM202",
                        format!("construct member `{method_name}` is declared more than once"),
                    );
                    continue;
                }
                methods.insert(
                    method_name,
                    ConstructFamilyMethod {
                        function: &method.function,
                        source,
                        computed: method.computed,
                        required: method.required,
                        result_declared: method.function.return_type.is_some(),
                        params: Vec::new(),
                        ownership: Vec::new(),
                        result: Type::Error,
                        uniform: false,
                        dispatcher: None,
                        defaults: Vec::new(),
                    },
                );
            }
            self.construct_family_names.insert(enum_id, name.clone());
            self.construct_families.insert(
                name,
                ConstructFamilyInfo {
                    enum_id,
                    required,
                    methods,
                    field_members,
                    variants: Vec::new(),
                    member_types,
                },
            );
        }
    }

    /// Declares and fills every construct-backed struct, then closes each family
    /// enum over those concrete variants.
    pub(crate) fn collect_constructs(&mut self) {
        let backed = self.backed_declarations();
        let mut declared = Vec::new();
        for (source, declaration) in backed {
            self.source = source;
            let name = self.interner.resolve(declaration.name).to_owned();
            let owner = self.imports.package_of(source).map(str::to_owned);
            match self.program.types.structs_mut().declare_owned(
                owner.as_deref(),
                StructDef {
                    name: name.clone(),
                    fields: Vec::new(),
                },
            ) {
                Some(id) => {
                    // A construct-backed declaration is a struct like any
                    // other, so where it was written gates who may name it.
                    self.struct_sources.insert(id, source);
                    declared.push((source, declaration, id));
                }
                None => self.emit(
                    declaration.name_span,
                    "KSEM004",
                    format!("`{name}` is already defined"),
                ),
            }
        }

        for (source, declaration, id) in declared {
            self.source = source;
            self.define_construct(declaration, id);
        }
        self.finish_family_variants();
        self.resolve_family_method_signatures();
        self.resolve_family_field_members();

        // Family templates have no runtime struct, but structural clauses still
        // receive their precise refusal.
        for (source, declaration) in self.family_declarations() {
            self.source = source;
            self.refuse_deferred(declaration);
        }
    }

    fn define_construct(&mut self, declaration: &ConstructDecl, id: StructId) {
        let ConstructKind::Backed {
            family,
            family_span,
            params,
        } = &declaration.kind
        else {
            return;
        };
        let name = self.interner.resolve(declaration.name).to_owned();
        let family_name = self.interner.resolve(*family).to_owned();

        let mut fields = Vec::new();
        let mut defaults = Vec::new();
        let mut seen = HashSet::new();
        for param in params {
            let field_name = self.interner.resolve(param.name).to_owned();
            self.note_duplicate_member(&mut seen, &field_name, param.name_span);
            fields.push(FieldDef {
                name: field_name,
                ty: self.resolve_type_ref(param.ty),
                mutable: false,
            });
            defaults.push(None);
        }
        let mut slots = Vec::new();
        for field in &declaration.fields {
            let field_name = self.interner.resolve(field.name).to_owned();
            self.note_duplicate_member(&mut seen, &field_name, field.name_span);
            let field_index = fields.len() as u32;
            let ty = if field.slot {
                self.resolve_slot_field(field, field_index, &field_name, &mut slots)
            } else {
                self.resolve_type_ref(field.ty)
            };
            fields.push(FieldDef {
                name: field_name,
                ty,
                mutable: false,
            });
            let default = if field.slot {
                None
            } else {
                field
                    .default
                    .map(|syntax| FieldDefault::new(syntax, self.source))
            };
            defaults.push(default);
        }

        let mut computed = HashSet::new();
        let mut own_methods = HashSet::new();
        for method in &declaration.methods {
            let member = self.interner.resolve(method.function.name).to_owned();
            // `@Required` states an obligation, which only a family can do: a
            // backed declaration is where an obligation is discharged, and a
            // bodyless member there would be an implementation that does
            // nothing.
            if method.required {
                self.emit(
                    method.function.name_span,
                    "KSEM249",
                    format!(
                        "`{name}` is a declaration backed by `{family_name}`, so `{member}` \
                         implements a requirement rather than declaring one; write it as an \
                         ordinary `function` member with a body"
                    ),
                );
                continue;
            }
            self.note_duplicate_member(&mut seen, &member, method.function.name_span);
            own_methods.insert(member.clone());
            if method.computed {
                computed.insert(member);
            }
        }

        let family_surface = self.construct_families.get(&family_name).map(|info| {
            // A uniform `extend` modifier has one shared body and is never
            // implemented per variant, so it is not part of the conformance
            // surface a backed declaration must satisfy.
            let methods = info
                .methods
                .iter()
                .filter(|(_, method)| !method.uniform)
                .map(|(name, method)| (name.clone(), method.computed))
                .collect::<Vec<_>>();
            (info.enum_id, info.required.clone(), methods)
        });
        let mut family = None;
        match family_surface {
            None => self.emit(
                *family_span,
                "KSEM200",
                format!("`{name}` is backed by unknown construct family `{family_name}`"),
            ),
            Some((enum_id, required, methods)) => {
                for (method, is_computed) in &methods {
                    if !own_methods.contains(method) && *is_computed {
                        computed.insert(method.clone());
                    }
                }
                let overrides_all_methods = !methods.is_empty()
                    && methods
                        .iter()
                        .all(|(method, _)| own_methods.contains(method));
                if !overrides_all_methods {
                    for required in required {
                        if !seen.contains(&required) {
                            self.emit(
                                declaration.name_span,
                                "KSEM201",
                                format!(
                                    "`{name}` does not provide required member `{required}` of \
                                     construct family `{family_name}`, and does not override every \
                                     family method that can consume it"
                                ),
                            );
                        }
                    }
                }
                if let Some(info) = self.construct_families.get_mut(&family_name) {
                    let tag = info.variants.len() as u32;
                    info.variants.push(ConstructVariant { struct_id: id, tag });
                    family = Some((enum_id, tag));
                }
            }
        }

        self.program.types.structs_mut().set_fields(id, fields);
        self.struct_defaults.push(defaults);
        self.constructs.insert(
            id,
            ConstructInfo {
                computed,
                slots,
                family,
            },
        );
        self.refuse_deferred(declaration);
    }

    fn resolve_slot_field(
        &mut self,
        field: &kira_syntax_model::ast::ConstructField,
        field_index: u32,
        field_name: &str,
        slots: &mut Vec<ContentSlot>,
    ) -> Type {
        let (element_ref, list) = match self.tree.type_ref(field.ty) {
            kira_syntax_model::ast::TypeRef::Array { element, .. } => (*element, true),
            _ => (field.ty, false),
        };
        let element_ty = self.resolve_type_ref(element_ref);
        let field_ty = if list {
            self.program.types.array_of(element_ty)
        } else {
            element_ty
        };
        slots.push(ContentSlot {
            field_index,
            name: field_name.to_owned(),
            list,
            element_ty,
            field_ty,
        });
        field_ty
    }

    fn finish_family_variants(&mut self) {
        let rows: Vec<_> = self
            .construct_families
            .values()
            .map(|info| {
                let variants = info
                    .variants
                    .iter()
                    .map(|variant| VariantDef {
                        name: self
                            .program
                            .types
                            .type_name(Type::Struct(variant.struct_id)),
                        payload: Some(Type::Struct(variant.struct_id)),
                    })
                    .collect();
                (info.enum_id, variants)
            })
            .collect();
        for (id, variants) in rows {
            self.program.types.enums_mut().set_variants(id, variants);
        }
    }

    /// Resolves the written type of every `@Required let` family member.
    ///
    /// Runs with the method signatures, after the family enums exist, because a
    /// requirement may name its own family — `@Required let body: Widget` on
    /// `construct Widget` is the ordinary case, not the exotic one.
    fn resolve_family_field_members(&mut self) {
        let rows: Vec<_> = self
            .construct_families
            .iter()
            .flat_map(|(family, info)| {
                info.field_members.keys().filter_map(move |name| {
                    let (type_ref, source) = *info.member_types.get(name)?;
                    Some((family.clone(), name.clone(), type_ref, source))
                })
            })
            .collect();
        for (family, name, type_ref, source) in rows {
            self.source = source;
            let result = self.resolve_type_ref(type_ref);
            if let Some(member) = self
                .construct_families
                .get_mut(&family)
                .and_then(|info| info.field_members.get_mut(&name))
            {
                member.result = result;
            }
        }
    }

    fn resolve_family_method_signatures(&mut self) {
        let rows: Vec<_> = self
            .construct_families
            .iter()
            .flat_map(|(family, info)| {
                info.methods.iter().map(move |(name, method)| {
                    (family.clone(), name.clone(), method.source, method.function)
                })
            })
            .collect();
        for (family, name, source, function) in rows {
            self.source = source;
            let params: Vec<Type> = function
                .params
                .iter()
                .map(|param| self.resolve_type_ref(param.ty))
                .collect();
            let ownership = function
                .params
                .iter()
                .map(|param| self.check_param_ownership(param))
                .collect();
            let result = function
                .return_type
                .map(|result| self.resolve_type_ref(result))
                .unwrap_or(Type::Void);
            if let Some(method) = self
                .construct_families
                .get_mut(&family)
                .and_then(|info| info.methods.get_mut(&name))
            {
                method.params = params;
                method.ownership = ownership;
                method.result = result;
            }
        }
    }

    /// Resolves every construct-family method's parameter defaults once, each in
    /// its declaring file.
    ///
    /// A family method (a per-variant method or a uniform `extend` modifier)
    /// carries no [`kira_semantics_model::hir::FuncId`] signature row, so its
    /// defaults cannot ride [`Analyzer::resolve_param_defaults`]. They are
    /// resolved here the same way — after signatures exist, in an empty scope,
    /// against each parameter's declared type — and reused by every call on a
    /// family value that omits the argument.
    pub(crate) fn resolve_construct_method_defaults(&mut self) {
        let rows: Vec<(String, String, SourceId, &'a Function, Vec<Type>)> = self
            .construct_families
            .iter()
            .flat_map(|(family, info)| {
                info.methods.iter().map(move |(name, method)| {
                    (
                        family.clone(),
                        name.clone(),
                        method.source,
                        method.function,
                        method.params.clone(),
                    )
                })
            })
            .collect();
        for (family, name, source, function, params) in rows {
            if function.params.iter().all(|param| param.default.is_none()) {
                continue;
            }
            self.source = source;
            let defaults: Vec<Option<HirExprId>> = function
                .params
                .iter()
                .enumerate()
                .map(|(index, param)| {
                    param
                        .default
                        .map(|syntax| self.analyze_default(syntax, params.get(index).copied()))
                })
                .collect();
            if let Some(method) = self
                .construct_families
                .get_mut(&family)
                .and_then(|info| info.methods.get_mut(&name))
            {
                method.defaults = defaults;
            }
        }
    }

    fn backed_declarations(&self) -> Vec<(SourceId, &'a ConstructDecl)> {
        let tree: &'a kira_syntax_model::SyntaxTree = self.tree;
        tree.items_with_source()
            .filter_map(|(source, item)| match item {
                Item::Construct(declaration)
                    if matches!(declaration.kind, ConstructKind::Backed { .. }) =>
                {
                    Some((source, declaration))
                }
                _ => None,
            })
            .collect()
    }

    fn family_declarations(&self) -> Vec<(SourceId, &'a ConstructDecl)> {
        let tree: &'a kira_syntax_model::SyntaxTree = self.tree;
        tree.items_with_source()
            .filter_map(|(source, item)| match item {
                Item::Construct(declaration)
                    if matches!(declaration.kind, ConstructKind::Family) =>
                {
                    Some((source, declaration))
                }
                _ => None,
            })
            .collect()
    }

    /// Registers own and inherited methods for one construct-backed declaration.
    pub(crate) fn construct_callables(
        &self,
        declaration: &'a ConstructDecl,
        source: SourceId,
        callables: &mut Vec<Callable<'a>>,
    ) {
        let ConstructKind::Backed { family, .. } = &declaration.kind else {
            return;
        };
        let name = self.interner.resolve(declaration.name);
        // The declaration's *own* struct, not a name written in some file: this
        // walk registers what this declaration provides, so it looks the
        // declaration up under its own package rather than by visibility.
        let owner = self.imports.package_of(source);
        let Some(id) = self.program.types.structs().lookup_owned(owner, name) else {
            return;
        };
        if !self.constructs.contains_key(&id) {
            return;
        }
        let mut own = HashSet::new();
        for method in &declaration.methods {
            // Refused in `define_construct`, and bodyless: registering it would
            // mint an implementation that does nothing.
            if method.required {
                continue;
            }
            own.insert(self.interner.resolve(method.function.name));
            callables.push(Callable {
                receiver: Some(id),
                origin: None,
                specialize: Vec::new(),
                function: &method.function,
                source,
            });
        }
        let family_name = self.interner.resolve(*family);
        if let Some(info) = self.construct_families.get(family_name) {
            for (method_name, method) in &info.methods {
                if own.contains(method_name.as_str()) {
                    continue;
                }
                // A `@Required function` has no body, so there is nothing to
                // inherit. Registering its empty block as this declaration's
                // implementation would make an unimplemented requirement look
                // satisfied; leaving it out is what lets the conformance check
                // see the gap.
                if method.required {
                    continue;
                }
                callables.push(Callable {
                    receiver: Some(id),
                    origin: None,
                    specialize: Vec::new(),
                    function: method.function,
                    source: method.source,
                });
            }
        }
    }

    fn note_duplicate_member(&mut self, seen: &mut HashSet<String>, name: &str, span: Span) {
        if !seen.insert(name.to_owned()) {
            self.emit(
                span,
                "KSEM202",
                format!("construct member `{name}` is declared more than once"),
            );
        }
    }

    fn refuse_deferred(&mut self, declaration: &ConstructDecl) {
        for deferred in &declaration.deferred {
            self.emit(
                deferred.span,
                "KSEM203",
                format!(
                    "{} is not executable yet in a construct; the executable slice supports \
                     `@Required let`, `@Required function`, stored and computed `let` members, \
                     `body {{ … }}`, `function`/`@Consuming function` members, and \
                     `some X` / `[some X]` child slots",
                    deferred.label
                ),
            );
        }
    }
}
