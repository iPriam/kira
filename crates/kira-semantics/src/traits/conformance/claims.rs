use super::*;

impl<'a> Analyzer<'a> {
    /// Records every conformance the program declares, from every spelling.
    ///
    /// Runs once every struct-shaped type has an id, because a conformance
    /// names one, and before callables are enumerated, because an inherited
    /// default becomes one.
    ///
    /// A construct family's claim is held back to a second pass, because it
    /// obliges declarations rather than declaring their conformance: one that
    /// writes the trait itself keeps its own, and the family's claim is then
    /// already satisfied for it.
    pub(crate) fn collect_conformances(&mut self) {
        let presented = self.presented_method_names();
        self.record_family_contracts();
        let mut family_claims: Vec<(SourceId, String, Vec<TraitRef>)> = Vec::new();
        let tree = self.tree;
        for (source, item) in tree.items_with_source() {
            self.source = source;
            match item {
                Item::Struct(declaration) if !declaration.traits.is_empty() => {
                    let name = self.interner.resolve(declaration.name);
                    if let Some(id) = self.declared_struct(name, source) {
                        self.declare_conformances(
                            &declaration.traits,
                            Type::Struct(id),
                            source,
                            &presented,
                        );
                    }
                }
                Item::Class(declaration) if !declaration.traits.is_empty() => {
                    let name = self.interner.resolve(declaration.name);
                    if let Some(id) = self.declared_struct(name, source) {
                        self.declare_conformances(
                            &declaration.traits,
                            Type::Struct(id),
                            source,
                            &presented,
                        );
                    }
                }
                Item::Construct(declaration) if !declaration.traits.is_empty() => {
                    match declaration.kind {
                        ConstructKind::Family => {
                            let name = self.interner.resolve(declaration.name).to_owned();
                            family_claims.push((source, name, declaration.traits.clone()));
                        }
                        ConstructKind::Backed { .. } => {
                            let name = self.interner.resolve(declaration.name);
                            if let Some(id) = self.declared_struct(name, source) {
                                self.declare_conformances(
                                    &declaration.traits,
                                    Type::Struct(id),
                                    source,
                                    &presented,
                                );
                            }
                        }
                    }
                }
                Item::Enum(declaration) if !declaration.traits.is_empty() => {
                    let name = self.interner.resolve(declaration.name);
                    if let Some(id) = self.visible_enum(name) {
                        self.declare_conformances(
                            &declaration.traits,
                            Type::Enum(id),
                            source,
                            &presented,
                        );
                    }
                }
                Item::Extend(declaration) => {
                    let Some(claimed) = declaration.conforms.as_ref() else {
                        continue;
                    };
                    let name = self.interner.resolve(declaration.name).to_owned();
                    if let Some(key) = self.visible_family_key(&name) {
                        family_claims.push((source, key, vec![claimed.clone()]));
                        continue;
                    }
                    match self.conforming_type_named(&name, source, declaration.target) {
                        Some(ty) => self.declare_conformances(
                            std::slice::from_ref(claimed),
                            ty,
                            source,
                            &presented,
                        ),
                        None => {
                            let trait_name = self.interner.resolve(claimed.name).to_owned();
                            self.emit(
                                declaration.name_span,
                                "KSEM298",
                                format!(
                                    "`{name}` is not a type that can conform to `{trait_name}`: \
                                     an impl block implements a trait for a struct, a class, or \
                                     a construct-backed declaration"
                                ),
                            );
                        }
                    }
                }
                _ => {}
            }
        }
        for (source, family, claimed) in family_claims {
            self.source = source;
            self.declare_family_conformances(&claimed, &family, source, &presented);
        }
    }

    /// Every method name each struct-shaped type presents, from every place one
    /// can be written for it.
    ///
    /// This is what decides which trait defaults a type still needs: a type
    /// that presents a member of the default's name has answered for it,
    /// whether it wrote the method in its own body, inherited it from a class
    /// parent or a construct family, or wrote it in an impl block.
    fn presented_method_names(&mut self) -> PresentedNames {
        let mut presented: PresentedNames = HashMap::new();
        for (source, item) in self.tree.items_with_source() {
            let (id, names) = match item {
                Item::Struct(declaration) => {
                    let name = self.interner.resolve(declaration.name);
                    let Some(id) = self.declared_struct(name, source) else {
                        continue;
                    };
                    (
                        Type::Struct(id),
                        self.method_names(declaration.methods.iter()),
                    )
                }
                Item::Class(declaration) => {
                    let name = self.interner.resolve(declaration.name);
                    let Some(id) = self.declared_struct(name, source) else {
                        continue;
                    };
                    let mut names =
                        self.method_names(declaration.methods.iter().map(|it| &it.function));
                    names.extend(self.inherited_method_names(id));
                    (Type::Struct(id), names)
                }
                Item::Construct(declaration) => {
                    let ConstructKind::Backed { .. } = declaration.kind else {
                        continue;
                    };
                    let name = self.interner.resolve(declaration.name);
                    let Some(id) = self.declared_struct(name, source) else {
                        continue;
                    };
                    let mut names =
                        self.method_names(declaration.methods.iter().map(|it| &it.function));
                    names.extend(self.family_method_names(declaration));
                    (Type::Struct(id), names)
                }
                Item::Extend(declaration) if declaration.conforms.is_some() => {
                    let name = self.interner.resolve(declaration.name).to_owned();
                    // Resolved against the *declaring* file, which is the scope
                    // the block's own header was written in.
                    let Some(ty) = self.conforming_type_named(&name, source, declaration.target)
                    else {
                        continue;
                    };
                    (ty, self.method_names(declaration.methods.iter()))
                }
                _ => continue,
            };
            presented.entry(id).or_default().extend(names);
        }
        presented
    }

    /// Records the contract every construct-backed declaration keeps: its
    /// family's own `@Required` surface.
    ///
    /// A family states requirements the way a trait does, so its declarations
    /// are conforming types in the same table. Filing them here is what lets
    /// one pass check both kinds, and what makes `conforms_to` answer for a
    /// family as well as for a trait.
    fn record_family_contracts(&mut self) {
        let sites = self.backed_declaration_sites();
        let rows: Vec<Conformance> = self
            .constructs
            .iter()
            .filter_map(|(ty, info)| {
                let (source, span) = sites.get(ty).copied()?;
                Some(Conformance {
                    contract: Contract::Family(info.family.clone()),
                    ty: Type::Struct(*ty),
                    source,
                    span,
                    via_family: None,
                    provided: info.members.clone(),
                })
            })
            .collect();
        self.conformances.extend(rows);
    }

    /// Records a construct family's conformance list, from either spelling.
    ///
    /// A family is a template rather than a type, so it keeps no promise
    /// itself: the claim is an obligation on every declaration backed by it,
    /// and it becomes one conformance per declaration — filed at that
    /// declaration, which is where a refusal's fix goes. A member the family
    /// itself provides answers for all of them, which is what makes
    /// `construct Widget: Hashable` worth writing rather than repeating the
    /// claim on each declaration.
    ///
    /// A declaration that already claims the trait keeps its own conformance:
    /// the family's claim states what must be true of it, and it is.
    fn declare_family_conformances(
        &mut self,
        claimed: &[TraitRef],
        family: &str,
        source: SourceId,
        presented: &PresentedNames,
    ) {
        let variants: Vec<StructId> = self
            .construct_families
            .get(family)
            .map(|info| {
                info.variants
                    .iter()
                    .filter_map(|it| it.struct_id())
                    .collect()
            })
            .unwrap_or_default();
        let sites = self.backed_declaration_sites();
        for entry in claimed {
            let written_name = self.interner.resolve(entry.name).to_owned();
            let Some(trait_name) = self.resolve_trait_ref(entry) else {
                if !is_builtin_trait(&written_name) && self.visible_trait_key(&written_name).is_none() {
                    self.emit(
                        entry.span,
                        "KSEM289",
                        format!(
                            "`{written_name}` is not a trait, so construct family `{family}` \
                             cannot claim it"
                        ),
                    );
                }
                continue;
            };
            if !self.family_may_claim(&trait_name, family, source, entry.span) {
                continue;
            }
            self.link_type_name(&trait_name, entry.span);
            for ty in &variants {
                let concrete = Type::Struct(*ty);
                if self.conforms_to(concrete, &trait_name) {
                    continue;
                }
                let (declared_in, at) = sites.get(ty).copied().unwrap_or((source, entry.span));
                self.conformances.push(Conformance {
                    contract: Contract::Trait(trait_name.clone()),
                    ty: concrete,
                    source: declared_in,
                    span: at,
                    via_family: Some(family.to_owned()),
                    provided: presented.get(&concrete).cloned().unwrap_or_default(),
                });
            }
        }
    }

    /// Whether a family claim written in `source` may name `trait_name`.
    ///
    /// The same two questions a type's own claim answers — is it a trait, and
    /// is the claim written where both parties can see it — asked once for the
    /// family rather than once per declaration backed by it.
    fn family_may_claim(
        &mut self,
        trait_name: &str,
        family: &str,
        source: SourceId,
        span: Span,
    ) -> bool {
        if is_builtin_trait(trait_name) {
            self.emit(
                span,
                "KSEM298",
                format!(
                    "a construct family cannot claim `{trait_name}`: it is a compiler-known trait \
                     about one type's own members, and a family is a template rather than a type. \
                     Claim it on each declaration backed by `{family}`."
                ),
            );
            return false;
        }
        if !self.traits.contains_key(trait_name) {
            self.emit(
                span,
                "KSEM289",
                format!(
                    "`{trait_name}` is not a trait, so construct family `{family}` cannot claim it"
                ),
            );
            return false;
        }
        let here = self.imports.package_of(source);
        let family_package = self
            .family_source(family)
            .map(|declared| self.imports.package_of(declared));
        let trait_package = self
            .traits
            .get(trait_name)
            .map(|declared| self.imports.package_of(declared.source));
        if Some(here) == family_package || Some(here) == trait_package {
            return true;
        }
        self.emit(
            span,
            "KSEM291",
            format!(
                "construct family `{family}` may conform to `{trait_name}` only where one of them \
                 is declared. A conformance written anywhere else would be invisible to every \
                 other user of both."
            ),
        );
        false
    }

    /// The file a construct family was declared in.
    fn family_source(&self, family: &str) -> Option<SourceId> {
        self.family_declarations()
            .into_iter()
            .find(|(_, declaration)| self.interner.resolve(declaration.name) == family)
            .map(|(source, _)| source)
    }

    /// Where each construct-backed declaration was written, keyed by its type.
    fn backed_declaration_sites(&self) -> HashMap<StructId, (SourceId, Span)> {
        self.backed_declarations()
            .into_iter()
            .filter_map(|(source, declaration)| {
                let name = self.interner.resolve(declaration.name);
                let id = self.declared_struct(name, source)?;
                Some((id, (source, declaration.name_span)))
            })
            .collect()
    }

    /// Records one declaration's conformance list, refusing what it may not
    /// claim.
    fn declare_conformances(
        &mut self,
        claimed: &[TraitRef],
        ty: Type,
        source: SourceId,
        presented: &PresentedNames,
    ) {
        for entry in claimed {
            let written_name = self.interner.resolve(entry.name).to_owned();
            let Some(trait_name) = self.resolve_trait_ref(entry) else {
                if !is_builtin_trait(&written_name) && self.visible_trait_key(&written_name).is_none() {
                    self.emit(
                        entry.span,
                        "KSEM289",
                        format!(
                            "`{written_name}` is not a trait, so the type cannot conform to it"
                        ),
                    );
                }
                continue;
            };
            let type_name = self.program.types.type_name(ty);
            if !is_builtin_trait(&trait_name) && !self.traits.contains_key(&trait_name) {
                self.emit(
                    entry.span,
                    "KSEM289",
                    format!("`{trait_name}` is not a trait, so `{type_name}` cannot conform to it"),
                );
                continue;
            }
            if self.conforms_to(ty, &trait_name) {
                self.emit(
                    entry.span,
                    "KSEM290",
                    format!(
                        "`{type_name}` already conforms to `{trait_name}`: one type keeps a \
                         trait's promise in exactly one way, so a second conformance would leave \
                         a call with no answer to pick"
                    ),
                );
                continue;
            }
            if !self.conformance_is_coherent(&trait_name, ty, source) {
                let owner = match ty {
                    Type::Struct(id) => self
                        .program
                        .types
                        .structs()
                        .owner_of(id)
                        .unwrap_or("this program")
                        .to_owned(),
                    _ => "this program".to_owned(),
                };
                self.emit(
                    entry.span,
                    "KSEM291",
                    format!(
                        "`{type_name}` may conform to `{trait_name}` only where one of them is \
                         declared — in `{owner}`, or in the package that declares \
                         `{trait_name}`. A conformance written anywhere else would be invisible \
                         to every other user of both."
                    ),
                );
                continue;
            }
            self.link_type_name(&trait_name, entry.span);
            self.conformances.push(Conformance {
                contract: Contract::Trait(trait_name),
                ty,
                source,
                span: entry.span,
                via_family: None,
                provided: presented.get(&ty).cloned().unwrap_or_default(),
            });
        }
    }

    /// Whether a conformance written in `source` may claim `trait_name` for
    /// `ty`.
    ///
    /// The rule is orphan-free by construction: the package that declares the
    /// type and the package that declares the trait each already have to know
    /// about the other for the conformance to be written at all, and every
    /// third package would be adding an answer neither of them can see.
    ///
    /// A compiler-known trait is declared by no package, so the type's own is
    /// the only one that may claim it.
    pub(crate) fn conformance_is_coherent(
        &self,
        trait_name: &str,
        ty: Type,
        source: SourceId,
    ) -> bool {
        let here = self.imports.package_of(source);
        if let Type::Struct(id) = ty
            && self.program.types.structs().owner_of(id) == here
        {
            return true;
        }
        self.traits
            .get(trait_name)
            .is_some_and(|declared| self.imports.package_of(declared.source) == here)
    }

    /// The struct `name` mints in the package `source` belongs to.
    ///
    /// A declaration registers what *it* provides, so the lookup is under its
    /// own package rather than under whatever a bare name would resolve to.
    fn declared_struct(&self, name: &str, source: SourceId) -> Option<StructId> {
        self.program
            .types
            .structs()
            .lookup_owned(self.imports.package_of(source), name)
    }

    /// Resolves an impl target to the concrete type it names, including scalar
    /// spellings, aliases, enums, and explicit generic type references.
    fn conforming_type_named(
        &mut self,
        name: &str,
        source: SourceId,
        target: Option<TypeRefId>,
    ) -> Option<Type> {
        let outer_source = self.source;
        self.source = source;
        let resolved = if let Some(target) = target {
            Some(self.resolve_type_ref(target))
        } else if let Some(ty) = Type::from_name(name) {
            (ty != Type::Void && ty != Type::Error && ty != Type::CString).then_some(ty)
        } else if let Some(ty) = self.resolve_alias_name(name, &crate::types::NameContext::Ordinary)
        {
            Some(ty)
        } else {
            self.visible_struct(name)
                .map(Type::Struct)
                .or_else(|| self.visible_enum(name).map(Type::Enum))
        };
        self.source = outer_source;
        resolved
    }

    /// The names of a run of declared methods.
    fn method_names<'f, I>(&self, functions: I) -> HashSet<String>
    where
        I: Iterator<Item = &'f Function>,
    {
        functions
            .map(|function| self.interner.resolve(function.name).to_owned())
            .collect()
    }

    /// Every method name a class inherits, which is as much its answer for a
    /// trait member as one it wrote itself.
    fn inherited_method_names(&self, id: StructId) -> HashSet<String> {
        let Some(info) = self.classes.get(&id) else {
            return HashSet::new();
        };
        info.bare_methods
            .keys()
            .map(|key| member_name(key).to_owned())
            .collect()
    }

    /// Every method name a construct-backed declaration takes from its family.
    fn family_method_names(
        &self,
        declaration: &kira_syntax_model::ast::ConstructDecl,
    ) -> HashSet<String> {
        let ConstructKind::Backed { family, .. } = declaration.kind else {
            return HashSet::new();
        };
        let family = self.interner.resolve(family);
        self.construct_families
            .get(family)
            .map(|info| info.methods.keys().cloned().collect())
            .unwrap_or_default()
    }

    /// Whether `ty` conforms to the trait `name`.
    pub(crate) fn conforms_to(&self, ty: Type, name: &str) -> bool {
        self.conformances
            .iter()
            .any(|entry| entry.ty == ty && entry.contract.trait_name() == Some(name))
    }

    /// Adds the callables conformance mints: an impl block's members, and every
    /// default a conforming type did not write itself.
    ///
    /// An inherited default is registered exactly the way a class registers an
    /// inherited method — one copy per type that gets it, with the receiver set
    /// to *that* type — so `self` inside the default's body is statically the
    /// concrete type and the call it makes resolves without dispatch.
    pub(crate) fn trait_callables(&self, callables: &mut Vec<Callable<'a>>) {
        for (source, item) in self.tree.items_with_source() {
            let Item::Extend(declaration) = item else {
                continue;
            };
            let Some(claimed) = declaration.conforms.as_ref() else {
                continue;
            };
            // `extend Family: Trait` is a modifier block, whose methods are
            // registered against the family and reached through the family
            // value; the conformances it minted are the backed declarations',
            // and one of those matching this block's span would add a second
            // callable for one body.
            let Some(entry) = self.conformances.iter().find(|entry| {
                entry.via_family.is_none() && entry.source == source && entry.span == claimed.span
            }) else {
                continue;
            };
            // A class already collects every `extend <Class>` block's methods
            // while it is flattened, so that a subclass inherits them. Adding
            // them again here would be two callables for one body.
            if let Type::Struct(id) = entry.ty
                && self.classes.contains_key(&id)
            {
                continue;
            }
            for method in &declaration.methods {
                callables.push(Callable {
                    receiver: Some(entry.ty),
                    origin: None,
                    specialize: Vec::new(),
                    initializes: None,
                    function: method,
                    source,
                    type_bindings: Vec::new(),
                });
            }
        }
        for entry in &self.conformances {
            let Some(declared) = entry
                .contract
                .trait_name()
                .and_then(|name| self.traits.get(name))
            else {
                continue;
            };
            for member in &declared.members {
                if member.required || entry.provided.contains(&member.name) {
                    continue;
                }
                callables.push(Callable {
                    receiver: Some(entry.ty),
                    origin: None,
                    specialize: Vec::new(),
                    initializes: None,
                    function: member.function,
                    // The default's body resolves names against the trait's own
                    // file, because that is where it was written.
                    source: declared.source,
                    type_bindings: declared.type_bindings.clone(),
                });
            }
        }
    }
}

/// The name part of a member key (`bump(Int)` names `bump`).
fn member_name(key: &str) -> &str {
    key.split_once('(').map_or(key, |(name, _)| name)
}
