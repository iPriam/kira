//! Declaration-time construct collection and two-phase family registration.

use std::collections::HashSet;

use kira_semantics_model::hir::HirExprId;
use kira_semantics_model::{EnumDef, StructDef, Type, VariantDef};
use kira_source::SourceId;
use kira_syntax_model::ast::{ConstructDecl, ConstructKind, Function, Item};

use super::{
    ConstructFamilyField, ConstructFamilyInfo, ConstructFamilyMethod, ConstructFamilyStoredField,
};
use crate::analyze::{Analyzer, Callable};

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
            let key = self.template_key(source, &name);
            if self.construct_families.contains_key(&key) {
                self.emit(
                    declaration.name_span,
                    "KSEM004",
                    format!("construct family `{name}` is already defined"),
                );
                continue;
            }
            let enum_name = format!("Any {name}");
            let owner = self.imports.package_of(source);
            let Some(enum_id) = self.program.types.enums_mut().declare_owned(
                owner,
                EnumDef {
                    name: enum_name,
                    variants: Vec::new(),
                },
            ) else {
                self.emit(
                    declaration.name_span,
                    "KSEM006",
                    format!("construct family type `Any {name}` is already defined"),
                );
                continue;
            };
            let module = self.imports.module_of(source).to_owned();
            self.program.types.enums_mut().set_module(enum_id, &module);
            self.enum_defaults.push(Vec::new());

            let required: Vec<String> = declaration
                .fields
                .iter()
                .filter(|field| field.required)
                .map(|field| self.interner.resolve(field.name).to_owned())
                .collect();
            // A family `let` is a *value* obligation, so it is readable through
            // the family value the way a computed member is. It gets its own map
            // rather than joining `methods` because it has no AST function
            // behind it: what discharges it is the backed declaration's choice
            // of a stored field or a computed member, and the dispatcher reads
            // whichever that declaration chose. A `@Required let` is guaranteed
            // on every variant by `KSEM201`; a defaulted stored member is
            // guaranteed because every backed struct materializes it — the
            // declaration's own field wins, otherwise the family default is
            // copied in. A stored member joins only with a written type: with
            // nothing declared, a family-value read has no result type to
            // stand on, and `KSEM271` names that at the read site. A child
            // slot is content wiring, not a value member.
            let field_members = declaration
                .fields
                .iter()
                .filter(|field| !field.slot && (field.required || field.ty.is_some()))
                .map(|field| {
                    (
                        self.interner.resolve(field.name).to_owned(),
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
                if let Some(ty) = field.ty {
                    member_types.insert(self.interner.resolve(field.name).to_owned(), (ty, source));
                } else if field.required {
                    self.emit(
                        field.name_span,
                        "KSEM261",
                        format!(
                            "required construct-family member `{}` must declare its type",
                            self.interner.resolve(field.name)
                        ),
                    );
                }
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
                // A family declares a *contract*, and a contract member is one
                // obligation per name: every backed declaration implements it,
                // and a family value dispatches to whichever implemented it.
                // Backed declarations may still overload their own members.
                if methods.contains_key(&method_name) {
                    self.emit(
                        method.function.name_span,
                        "KSEM202",
                        format!(
                            "construct family `{name}` already declares a member `{method_name}`: \
                             a family member is one obligation per name, so it is not overloadable"
                        ),
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
            let stored_fields = declaration
                .fields
                .iter()
                .filter(|field| !field.required)
                .map(|field| ConstructFamilyStoredField {
                    name: self.interner.resolve(field.name).to_owned(),
                    ty: field.ty,
                    default: field.default,
                    source,
                    slot: field.slot,
                    mutable: field.mutable,
                })
                .collect();
            self.construct_family_names.insert(enum_id, key.clone());
            self.construct_families.insert(
                key,
                ConstructFamilyInfo {
                    enum_id,
                    required,
                    methods,
                    field_members,
                    stored_fields,
                    variants: Vec::new(),
                    parents: Vec::new(),
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
                    c_layout: false,
                    drop_glue: None,
                },
            ) {
                Some(id) => {
                    let module = self.imports.module_of(source).to_owned();
                    self.program.types.structs_mut().set_module(id, &module);
                    // A construct-backed declaration is a struct like any
                    // other, so where it was written gates who may name it.
                    self.struct_sources.insert(id, source);
                    // Reserve the defaults slot now, in id order, exactly as
                    // `collect_structs` does: a function type minted while
                    // `define_construct` resolves a parameter (which pushes its
                    // own slot) must not land on this struct's id.
                    self.struct_defaults.push(Vec::new());
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
        self.check_family_overrides();

        // Family templates have no runtime struct, but structural clauses still
        // receive their precise refusal.
        for (source, declaration) in self.family_declarations() {
            self.source = source;
            self.refuse_deferred(declaration);
        }
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
                        name: self.program.types.type_name(variant.ty),
                        payload: Some(variant.ty),
                    })
                    .collect();
                (info.enum_id, variants)
            })
            .collect();
        for (id, variants) in rows {
            self.program.types.enums_mut().set_variants(id, variants);
        }
    }

    /// Resolves the written type of every family value member — `@Required let`
    /// and typed stored member alike.
    ///
    /// Runs with the method signatures, after the family enums exist, because a
    /// requirement may name its own family — `@Required let body: Any Widget` on
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

    pub(crate) fn backed_declarations(&self) -> Vec<(SourceId, &'a ConstructDecl)> {
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

    pub(crate) fn family_declarations(&self) -> Vec<(SourceId, &'a ConstructDecl)> {
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
        // Each `init(…)` is a free function producing the declaration. They all
        // share one name — see `Analyzer::initializer_name` — so the overload
        // set under it is exactly this declaration's secondary initializers.
        for init in &declaration.inits {
            callables.push(Callable {
                receiver: None,
                origin: None,
                specialize: Vec::new(),
                initializes: Some(id),
                function: init,
                source,
                type_bindings: Vec::new(),
            });
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
                receiver: Some(Type::Struct(id)),
                origin: None,
                specialize: Vec::new(),
                initializes: None,
                function: &method.function,
                source,
                type_bindings: Vec::new(),
            });
        }
        let family_name = self.interner.resolve(*family);
        if let Some(info) = self
            .visible_family_key(family_name)
            .and_then(|key| self.construct_families.get(&key))
        {
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
                    receiver: Some(Type::Struct(id)),
                    origin: None,
                    specialize: Vec::new(),
                    initializes: None,
                    function: method.function,
                    source: method.source,
                    type_bindings: Vec::new(),
                });
            }
        }
    }
}
