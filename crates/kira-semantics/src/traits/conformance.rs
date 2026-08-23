//! Collecting trait declarations and the conformances that claim them.
//!
//! Two passes, at two different moments. Traits are collected from syntax
//! alone, before any type table exists, because a trait declaration mentions no
//! type until its members are resolved — and because every later declaration
//! has to be able to say "that name is a trait". Conformances are collected
//! once every struct-shaped type has an id, because a conformance names one.

use std::collections::{HashMap, HashSet};

use kira_semantics_model::{StructId, Type};
use kira_source::{SourceId, Span};
use kira_syntax_model::ast::{ConstructKind, Function, Item, TraitRef};

use super::{Conformance, TraitInfo, TraitMemberInfo, is_builtin_trait};
use crate::analyze::{Analyzer, Callable};

/// Every method name each type presents, keyed by the type.
type PresentedNames = HashMap<StructId, HashSet<String>>;

impl<'a> Analyzer<'a> {
    /// Registers every `trait` declaration the program writes.
    ///
    /// Runs before any type table is built: a trait's *name* has to be known
    /// while structs, classes, enums, and construct families are being
    /// declared, because Kira has one type namespace and each of those has to
    /// be able to lose a collision to a trait.
    pub(crate) fn collect_traits(&mut self) {
        let tree = self.tree;
        for (source, item) in tree.items_with_source() {
            let Item::Trait(declaration) = item else {
                continue;
            };
            self.source = source;
            let name = self.interner.resolve(declaration.name).to_owned();
            for supertrait in &declaration.supertraits {
                let super_name = self.interner.resolve(supertrait.name).to_owned();
                self.emit(
                    supertrait.span,
                    "KSEM296",
                    format!(
                        "a trait cannot require another: `{name}` may not name `{super_name}` as \
                         a supertrait. Declare the members `{name}` needs on `{name}` itself, or \
                         have each conforming type claim both traits."
                    ),
                );
            }
            if is_builtin_trait(&name) {
                self.emit(
                    declaration.name_span,
                    "KSEM288",
                    format!(
                        "`{name}` is a trait the compiler already knows, so it cannot be \
                         declared: what it means is decided by a type's own members, not by a \
                         written body"
                    ),
                );
                continue;
            }
            if let Some(kind) = self.name_declared_elsewhere(&name, source, declaration.name_span) {
                self.emit(
                    declaration.name_span,
                    "KSEM288",
                    format!(
                        "`{name}` is already defined as {kind}, so this trait cannot take the \
                         same name: a type name means exactly one declaration"
                    ),
                );
                continue;
            }
            let members = declaration
                .members
                .iter()
                .map(|member| TraitMemberInfo {
                    name: self.interner.resolve(member.function.name).to_owned(),
                    required: !member.has_body,
                    function: &member.function,
                })
                .collect();
            self.traits.insert(name, TraitInfo { source, members });
        }
    }

    /// What else already declares `name` in the package `source` belongs to, or
    /// `None` when the name is free there.
    ///
    /// Asked of the *syntax* rather than of a type table, because traits are
    /// collected before any table exists. `span` identifies the declaration
    /// asking, so a trait does not report itself.
    fn name_declared_elsewhere(
        &self,
        name: &str,
        source: SourceId,
        span: Span,
    ) -> Option<&'static str> {
        let package = self.imports.package_of(source);
        self.tree.items_with_source().find_map(|(other, item)| {
            if self.imports.package_of(other) != package {
                return None;
            }
            let (kind, declared, declared_span) = match item {
                Item::Struct(it) => ("a struct", it.name, it.name_span),
                Item::Class(it) => ("a class", it.name, it.name_span),
                Item::Enum(it) => ("an enum", it.name, it.name_span),
                Item::TypeAlias(it) => ("a type alias", it.name, it.name_span),
                Item::Construct(it) => match it.kind {
                    ConstructKind::Family => ("a construct family", it.name, it.name_span),
                    ConstructKind::Backed { .. } => ("a declaration", it.name, it.name_span),
                },
                Item::Trait(it) => ("a trait", it.name, it.name_span),
                _ => return None,
            };
            let repeat = declared_span == span && other == source;
            (!repeat && self.interner.resolve(declared) == name).then_some(kind)
        })
    }

    /// Records every conformance the program declares, from both spellings.
    ///
    /// Runs once every struct-shaped type has an id, because a conformance
    /// names one, and before callables are enumerated, because an inherited
    /// default becomes one.
    pub(crate) fn collect_conformances(&mut self) {
        let presented = self.presented_method_names();
        let tree = self.tree;
        for (source, item) in tree.items_with_source() {
            self.source = source;
            match item {
                Item::Struct(declaration) if !declaration.traits.is_empty() => {
                    let name = self.interner.resolve(declaration.name);
                    if let Some(id) = self.declared_struct(name, source) {
                        self.declare_conformances(&declaration.traits, id, source, &presented);
                    }
                }
                Item::Class(declaration) if !declaration.traits.is_empty() => {
                    let name = self.interner.resolve(declaration.name);
                    if let Some(id) = self.declared_struct(name, source) {
                        self.declare_conformances(&declaration.traits, id, source, &presented);
                    }
                }
                Item::Construct(declaration) if !declaration.traits.is_empty() => match declaration
                    .kind
                {
                    ConstructKind::Family => self.refuse_family_conformance(&declaration.traits),
                    ConstructKind::Backed { .. } => {
                        let name = self.interner.resolve(declaration.name);
                        if let Some(id) = self.declared_struct(name, source) {
                            self.declare_conformances(&declaration.traits, id, source, &presented);
                        }
                    }
                },
                Item::Extend(declaration) => {
                    let Some(claimed) = declaration.conforms else {
                        continue;
                    };
                    let name = self.interner.resolve(declaration.name).to_owned();
                    match self.visible_struct(&name) {
                        Some(id) => self.declare_conformances(
                            std::slice::from_ref(&claimed),
                            id,
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
    }

    /// Every method name each struct-shaped type presents, from every place one
    /// can be written for it.
    ///
    /// This is what decides which trait defaults a type still needs: a type
    /// that presents a member of the default's name has answered for it,
    /// whether it wrote the method in its own body, inherited it from a class
    /// parent or a construct family, or wrote it in an impl block.
    fn presented_method_names(&self) -> PresentedNames {
        let mut presented: PresentedNames = HashMap::new();
        for (source, item) in self.tree.items_with_source() {
            let (id, names) = match item {
                Item::Struct(declaration) => {
                    let name = self.interner.resolve(declaration.name);
                    let Some(id) = self.declared_struct(name, source) else {
                        continue;
                    };
                    (id, self.method_names(declaration.methods.iter()))
                }
                Item::Class(declaration) => {
                    let name = self.interner.resolve(declaration.name);
                    let Some(id) = self.declared_struct(name, source) else {
                        continue;
                    };
                    let mut names =
                        self.method_names(declaration.methods.iter().map(|it| &it.function));
                    names.extend(self.inherited_method_names(id));
                    (id, names)
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
                    (id, names)
                }
                Item::Extend(declaration) if declaration.conforms.is_some() => {
                    let name = self.interner.resolve(declaration.name).to_owned();
                    // Resolved against the *declaring* file, which is the scope
                    // the block's own header was written in.
                    let Some(id) = self.struct_visible_from(&name, source) else {
                        continue;
                    };
                    (id, self.method_names(declaration.methods.iter()))
                }
                _ => continue,
            };
            presented.entry(id).or_default().extend(names);
        }
        presented
    }

    /// Refuses a conformance list written on a construct family template.
    fn refuse_family_conformance(&mut self, claimed: &[TraitRef]) {
        for entry in claimed {
            let trait_name = self.interner.resolve(entry.name).to_owned();
            self.emit(
                entry.span,
                "KSEM298",
                format!(
                    "a construct family cannot claim `{trait_name}`: a family is a template \
                     rather than a type, so there is no value for the trait's members to run \
                     on. Claim the trait on each declaration backed by the family."
                ),
            );
        }
    }

    /// Records one declaration's conformance list, refusing what it may not
    /// claim.
    fn declare_conformances(
        &mut self,
        claimed: &[TraitRef],
        ty: StructId,
        source: SourceId,
        presented: &PresentedNames,
    ) {
        for entry in claimed {
            let trait_name = self.interner.resolve(entry.name).to_owned();
            let type_name = self.program.types.type_name(Type::Struct(ty));
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
                let owner = self
                    .program
                    .types
                    .structs()
                    .owner_of(ty)
                    .unwrap_or("this program")
                    .to_owned();
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
                trait_name,
                ty,
                source,
                span: entry.span,
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
    fn conformance_is_coherent(&self, trait_name: &str, ty: StructId, source: SourceId) -> bool {
        let here = self.imports.package_of(source);
        if self.program.types.structs().owner_of(ty) == here {
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

    /// The struct `name` denotes from `source`, whichever package declared it.
    fn struct_visible_from(&self, name: &str, source: SourceId) -> Option<StructId> {
        self.declared_struct(name, source)
            .or_else(|| self.program.types.structs().lookup(name))
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
    pub(crate) fn conforms_to(&self, ty: StructId, name: &str) -> bool {
        self.conformances
            .iter()
            .any(|entry| entry.ty == ty && entry.trait_name == name)
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
            let Some(claimed) = declaration.conforms else {
                continue;
            };
            let Some(entry) = self
                .conformances
                .iter()
                .find(|entry| entry.source == source && entry.span == claimed.span)
            else {
                continue;
            };
            // A class already collects every `extend <Class>` block's methods
            // while it is flattened, so that a subclass inherits them. Adding
            // them again here would be two callables for one body.
            if self.classes.contains_key(&entry.ty) {
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
                });
            }
        }
        for entry in &self.conformances {
            let Some(declared) = self.traits.get(&entry.trait_name) else {
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
                });
            }
        }
    }
}

/// The name part of a member key (`bump(Int)` names `bump`).
fn member_name(key: &str) -> &str {
    key.split_once('(').map_or(key, |(name, _)| name)
}
